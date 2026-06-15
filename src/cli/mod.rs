pub mod agent;
mod agent_provider;
mod agent_redact;
mod agent_review;
pub mod alias;
pub mod api;
pub mod auth;
mod auth_login;
mod auth_probe;
mod auth_setup_git;
pub mod branch;
pub mod browse;
pub mod config;
pub mod context;
pub mod editor;
pub mod extension;
pub mod hook;
pub mod instances;
pub mod issue;
mod issue_comment;
pub mod key;
pub mod label;
mod list;
pub mod milestone;
pub mod org;
pub mod pr;
mod pr_args;
pub(crate) mod pr_comment;
mod pr_inspect;
pub(crate) mod pr_merge_check;
mod pr_review;
mod pr_status;
pub mod protect;
pub mod release;
pub mod repo;
mod repo_deploy_key;
mod repo_mirror;
mod repo_social;
pub mod search;
pub mod stack;
mod stack_state;
pub mod status;
pub mod tag;
pub mod web;
pub mod work;
mod work_session;
pub mod workflow;
mod workflow_artifact;
mod workflow_run;
mod workflow_secret;
mod workflow_variable;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::output::ColorChoice;

/// `fj` — command-line tool for Forgejo.
#[derive(Debug, Parser)]
#[command(
    name = "fj",
    version,
    about = "Command-line tool for Forgejo",
    long_about = "fj is a command-line tool for Forgejo, in the spirit of GitHub's gh. It spans \
multiple hosts, stores tokens in your OS keychain or a 0600 file fallback, and mirrors the gh command surface for what \
Forgejo exposes: repositories, issues, pull requests, releases, and Actions (runs, workflows, \
secrets, variables), plus stacked-PR and working-session helpers.\n\
\n\
Most subcommands accept -R <owner>/<name> and otherwise infer the repository from the current \
git remote (preferring `upstream`, then `origin`). Authenticate once with `fj auth login`. Global \
flags --host, --debug, --color, --no-pager, and --json-fields work on every command.\n\
\n\
Examples:\n\
\n\
fj auth login: authenticate against a Forgejo host.\n\
\n\
fj repo clone owner/name: clone a repository over git.\n\
\n\
fj pr create: open a pull request from the current branch.\n\
\n\
fj pr list: list pull requests for the current repo.\n\
\n\
fj run watch: stream the latest Actions run's logs live.\n\
\n\
fj api /user: make a raw authenticated API request.",
    propagate_version = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Override the host for this command (otherwise the current host from
    /// `fj auth login` is used).
    #[arg(long, global = true, env = "FJ_HOST")]
    pub host: Option<String>,

    /// Log every HTTP request fj makes to stderr.
    #[arg(long, global = true, env = "FJ_DEBUG")]
    pub debug: bool,

    /// When to use ANSI color in output.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    pub color: ColorChoice,

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
    /// List and select the Forgejo instances owned by your Fjord Account.
    Instances(instances::InstancesCmd),
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
    /// Manage git tags.
    Tag(tag::TagCmd),
    /// Create a branch on Forgejo.
    BranchCreate(branch::BranchCreateArgs),
    /// Delete a branch on Forgejo.
    BranchDelete(branch::BranchDeleteArgs),
    /// Manage workflow runs (Forgejo Actions).
    Run(workflow::RunCmd),
    /// Manage workflow definitions (trigger a `workflow_dispatch`).
    Workflow(workflow::WorkflowCmd),
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
    /// Make a raw API request (like `gh api`).
    Api(api::ApiArgs),
    /// Inspect and resume your working context / session.
    Work(work::WorkCmd),
    /// Manage stacked branches / stacked PRs.
    Stack(stack::StackCmd),
    /// AI-assisted code operations (review, fix, explain).
    Agent(agent::AgentCmd),
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
    crate::output::set_color_choice(cli.color, config::no_color_enabled());
    crate::output::set_json_fields(cli.json_fields.as_deref());
    // Page commands whose output can run long. Pass `cli.no_pager` through
    // explicitly so we don't have to mutate the process environment to opt
    // out, which would be a process-wide side effect and an unsafe set_var.
    let _pager = match &cli.command {
        // `run watch` streams live; paging would buffer it. Never page it.
        Command::Run(rc) if matches!(rc.command, workflow_run::RunSub::Watch(_)) => None,
        Command::Repo(_)
        | Command::Issue(_)
        | Command::Pr(_)
        | Command::Release(_)
        | Command::Search(_)
        | Command::Status(_)
        | Command::Label(_)
        | Command::Milestone(_)
        | Command::Tag(_)
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
        Command::Instances(cmd) => instances::run(cmd, host).await,
        Command::Repo(cmd) => repo::run(cmd, host).await,
        Command::Issue(cmd) => issue::run(cmd, host).await,
        Command::Pr(cmd) => pr::run(cmd, host).await,
        Command::Release(cmd) => release::run(cmd, host).await,
        Command::Label(cmd) => label::run(cmd, host).await,
        Command::Milestone(cmd) => milestone::run(cmd, host).await,
        Command::Tag(cmd) => tag::run(cmd, host).await,
        Command::BranchCreate(args) => branch::create(args, host).await,
        Command::BranchDelete(args) => branch::delete(args, host).await,
        Command::Run(cmd) => workflow_run::run(cmd, host).await,
        Command::Workflow(cmd) => workflow::run_workflow(cmd, host).await,
        Command::Secret(cmd) => workflow_secret::run_secret(cmd, host).await,
        Command::Variable(cmd) => workflow_variable::run_variable(cmd, host).await,
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
        Command::Api(args) => api::run(args, host).await,
        Command::Work(cmd) => work::run(cmd, host).await,
        Command::Stack(cmd) => stack::run(cmd, host).await,
        Command::Agent(cmd) => agent::run(cmd, host).await,
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    /// Walk the whole derived command tree and assert it is internally
    /// consistent: no duplicate short flags, conflicting args, bad value
    /// parsers, or malformed subcommands. clap only performs these checks
    /// behind `debug_assert`, so without this test a broken arg definition
    /// in any command module ships and only fails the first time a user
    /// runs that subcommand. This guards every `cli/*.rs` at once, including
    /// the command modules excluded from coverage.
    #[test]
    fn command_tree_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn body_file_flags_parse_on_body_taking_commands() {
        let cli = Cli::try_parse_from([
            "fj",
            "issue",
            "create",
            "--title",
            "t",
            "--body-file",
            "body.md",
        ])
        .expect("issue create parses");
        match cli.command {
            Command::Issue(issue::IssueCmd {
                command: issue::IssueSub::Create(args),
            }) => assert_eq!(args.body_file.as_deref(), Some("body.md")),
            other => panic!("unexpected command: {other:?}"),
        }

        let cli = Cli::try_parse_from(["fj", "issue", "edit", "1", "-F", "-"])
            .expect("issue edit parses");
        match cli.command {
            Command::Issue(issue::IssueCmd {
                command: issue::IssueSub::Edit(args),
            }) => assert_eq!(args.body_file.as_deref(), Some("-")),
            other => panic!("unexpected command: {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "fj", "pr", "create", "--title", "t", "--head", "h", "-F", "-",
        ])
        .expect("pr create parses");
        match cli.command {
            Command::Pr(pr::PrCmd {
                command: pr::PrSub::Create(args),
            }) => assert_eq!(args.body_file.as_deref(), Some("-")),
            other => panic!("unexpected command: {other:?}"),
        }

        let cli = Cli::try_parse_from(["fj", "pr", "edit", "1", "--body-file", "body.md"])
            .expect("pr edit parses");
        match cli.command {
            Command::Pr(pr::PrCmd {
                command: pr::PrSub::Edit(args),
            }) => assert_eq!(args.body_file.as_deref(), Some("body.md")),
            other => panic!("unexpected command: {other:?}"),
        }

        let cli = Cli::try_parse_from(["fj", "release", "create", "v1", "-F", "body.md"])
            .expect("release create parses");
        match cli.command {
            Command::Release(release::ReleaseCmd {
                command: release::ReleaseSub::Create(args),
            }) => assert_eq!(args.body_file.as_deref(), Some("body.md")),
            other => panic!("unexpected command: {other:?}"),
        }

        let cli =
            Cli::try_parse_from(["fj", "milestone", "create", "v1", "--body-file", "body.md"])
                .expect("milestone create parses");
        match cli.command {
            Command::Milestone(milestone::MilestoneCmd {
                command: milestone::MilestoneSub::Create(args),
            }) => assert_eq!(args.body_file.as_deref(), Some("body.md")),
            other => panic!("unexpected command: {other:?}"),
        }

        let cli = Cli::try_parse_from(["fj", "milestone", "edit", "1", "-F", "-"])
            .expect("milestone edit parses");
        match cli.command {
            Command::Milestone(milestone::MilestoneCmd {
                command: milestone::MilestoneSub::Edit(args),
            }) => assert_eq!(args.body_file.as_deref(), Some("-")),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    /// Every subcommand (and nested subcommand) must render its help and
    /// long-help without panicking. `--help` at the top level only touches
    /// the first layer; this reaches the leaves.
    #[test]
    fn every_subcommand_renders_help() {
        fn walk(cmd: &mut clap::Command) {
            let _ = cmd.render_help();
            let _ = cmd.render_long_help();
            let names: Vec<String> = cmd
                .get_subcommands()
                .map(|s| s.get_name().to_string())
                .collect();
            for name in names {
                if let Some(sub) = cmd.find_subcommand_mut(&name) {
                    walk(sub);
                }
            }
        }
        walk(&mut Cli::command());
    }

    /// Automation scripts the read-only surface (`view` / `list` / inspect
    /// commands) and expects `--json`. Walk the whole tree and assert every
    /// leaf subcommand whose name marks it as a read/inspect/list surface
    /// either accepts `--json` or is explicitly allow-listed as text-only, so
    /// the surface can't silently regress (the `pr view` / rest-of-`pr` trap
    /// that motivated rasterstate/fj#112).
    #[test]
    fn read_surface_subcommands_accept_json_or_are_text_only() {
        // Leaf names that denote a structured read/inspect/list surface.
        const READ_SURFACE: &[&str] = &[
            "view", "list", "diff", "files", "commits", "checks", "status",
        ];
        // Full `fj <...>` paths intentionally kept text-only. Add here only
        // with a reason; everything else in READ_SURFACE must accept --json.
        const TEXT_ONLY: &[&str] = &[
            // `auth status` reports the active host/token as prose, not a
            // queryable record set.
            "fj auth status",
            // Local host/credential and config listings that have no JSON
            // branch today. Surfacing them is out of scope for #112; listed
            // here so the guard reflects reality without silently passing.
            "fj auth list",
            "fj alias list",
            "fj config list",
            "fj extension list",
            // `fj instances` exposes `--json` on the parent command; the bare
            // `list` leaf delegates to it rather than redefining the flag.
            "fj instances list",
        ];

        fn accepts_json(cmd: &clap::Command) -> bool {
            cmd.get_arguments().any(|a| a.get_long() == Some("json"))
        }

        fn walk(cmd: &mut clap::Command, path: &str, offenders: &mut Vec<String>) {
            let names: Vec<String> = cmd
                .get_subcommands()
                .map(|s| s.get_name().to_string())
                .collect();
            for name in names {
                let child_path = format!("{path} {name}");
                if let Some(sub) = cmd.find_subcommand_mut(&name) {
                    let is_leaf = sub.get_subcommands().next().is_none();
                    if is_leaf
                        && READ_SURFACE.contains(&name.as_str())
                        && !accepts_json(sub)
                        && !TEXT_ONLY.contains(&child_path.as_str())
                    {
                        offenders.push(child_path.clone());
                    }
                    walk(sub, &child_path, offenders);
                }
            }
        }

        let mut offenders = Vec::new();
        walk(&mut Cli::command(), "fj", &mut offenders);
        assert!(
            offenders.is_empty(),
            "read-only subcommands missing `--json` (add the flag, or allow-list \
             as text-only in TEXT_ONLY with a reason): {offenders:?}"
        );
    }

    /// `KNOWN_SUBCOMMANDS` in `main.rs` is a hand-maintained allowlist that
    /// `detect_plugin` consults so a `fj-<name>` binary on PATH can't shadow a
    /// real subcommand. It must match the derived command tree exactly; a
    /// missing entry (as `man` once was) silently lets a same-named plugin
    /// hijack the built-in. clap injects its own `help` subcommand, which is
    /// not a plugin concern, so it's exempt on both sides.
    #[test]
    fn known_subcommands_matches_the_command_tree() {
        use std::collections::BTreeSet;
        let derived: BTreeSet<String> = Cli::command()
            .get_subcommands()
            .map(|s| s.get_name().to_string())
            .filter(|n| n != "help")
            .collect();
        let allow: BTreeSet<String> = crate::KNOWN_SUBCOMMANDS
            .iter()
            .filter(|n| **n != "help")
            .map(|s| s.to_string())
            .collect();
        let missing: Vec<&String> = derived.difference(&allow).collect();
        let stale: Vec<&String> = allow.difference(&derived).collect();
        assert!(
            missing.is_empty() && stale.is_empty(),
            "KNOWN_SUBCOMMANDS is out of sync with the command tree:\n  \
             missing (add to main.rs KNOWN_SUBCOMMANDS): {missing:?}\n  \
             stale (remove from main.rs KNOWN_SUBCOMMANDS): {stale:?}"
        );
    }

    #[test]
    fn readme_global_flags_match_clap_globals() {
        use std::collections::BTreeSet;
        use std::fs;

        let clap_globals: BTreeSet<String> = Cli::command()
            .get_arguments()
            .filter(|arg| arg.is_global_set())
            .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
            .collect();

        let readme = fs::read_to_string(format!("{}/README.md", env!("CARGO_MANIFEST_DIR")))
            .expect("read README.md");
        let paragraph = readme
            .split("\n\n")
            .find(|paragraph| paragraph.starts_with("**Global flags**"))
            .expect("README.md global flags paragraph");
        let readme_globals = markdown_flags(paragraph);

        assert_eq!(
            readme_globals, clap_globals,
            "README.md global flags paragraph must match clap global=true args"
        );
    }

    fn markdown_flags(markdown: &str) -> std::collections::BTreeSet<String> {
        markdown
            .split('`')
            .skip(1)
            .step_by(2)
            .filter(|code| code.starts_with("--"))
            .map(ToString::to_string)
            .collect()
    }

    /// Keep the coverage scope honest: the `COV_IGNORE` list in the Makefile
    /// must stay in sync with reality. A `cli/*.rs` with no `#[cfg(test)]`
    /// module is glue we don't unit-test and must be excluded; one that has
    /// a test module is a unit-test target and must NOT be excluded. Without
    /// this guard the coverage number silently drifts as files are added.
    ///
    /// `mod.rs` is the one documented exception: it stays excluded as
    /// entry/dispatch even though it carries the structural tests above.
    #[test]
    fn cov_ignore_stays_in_sync_with_test_modules() {
        use std::collections::BTreeSet;
        use std::fs;

        let root = env!("CARGO_MANIFEST_DIR");

        let makefile = fs::read_to_string(format!("{root}/Makefile")).expect("read Makefile");
        let cov_line = makefile
            .lines()
            .find(|l| l.starts_with("COV_IGNORE"))
            .expect("COV_IGNORE assignment in Makefile");
        // Pull the `cli/( ... )` alternation and split it into basenames.
        let cli_group = cov_line
            .split_once("cli/(")
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(group, _)| group)
            .expect("cli/(...) group in COV_IGNORE");
        let excluded: BTreeSet<&str> = cli_group.split('|').collect();

        let mut problems = Vec::new();
        for entry in fs::read_dir(format!("{root}/src/cli")).expect("read src/cli") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let stem = path.file_stem().unwrap().to_str().unwrap().to_string();
            if stem == "mod" {
                continue; // documented exception (see doc comment)
            }
            let src = fs::read_to_string(&path).unwrap();
            let has_tests = src.contains("#[cfg(test)]");
            let is_excluded = excluded.contains(stem.as_str());
            if has_tests && is_excluded {
                problems.push(format!(
                    "cli/{stem}.rs has a #[cfg(test)] module but is in COV_IGNORE: \
                     remove it from COV_IGNORE (it is a unit-test target)"
                ));
            }
            if !has_tests && !is_excluded {
                problems.push(format!(
                    "cli/{stem}.rs has no #[cfg(test)] module and is not in COV_IGNORE: \
                     add it to COV_IGNORE (it is untested glue) or give it tests"
                ));
            }
        }
        assert!(
            problems.is_empty(),
            "COV_IGNORE is out of sync with the cli/ test modules:\n  {}",
            problems.join("\n  ")
        );
    }
}
