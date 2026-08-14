//! Which subcommands get their output piped through the pager.
//!
//! `output::pager::maybe_start` already declines to page when stdout is not a
//! terminal, when `FJ_NO_PAGER` is set, or when `--no-pager` was passed. This
//! module answers the other half of the question: for a command that *could*
//! page, *should* it?
//!
//! The rule is: page only genuinely long, read-only, human-facing list and
//! view output. Everything else writes straight to the terminal. Concretely we
//! do not page
//!
//! - status and summary commands (`fj status`, `fj pr status`), which print a
//!   short fixed block,
//! - writes and their confirmations (`create`, `merge`, `close`, `delete`,
//!   `star`, ...), which print a line or two and sometimes read an interactive
//!   confirmation from the very terminal we would have redirected into `less`,
//! - single-value or short reads (`tag view`, `milestone view`,
//!   `repo topics`, `repo set-default`),
//! - machine output: every `--json` mode, plus `--web`, which prints nothing
//!   and opens a browser.
//!
//! The predicate is a pure function of the parsed command tree so it can be
//! unit tested without a terminal.

use reqwest::Method;

use super::{
    api, issue, label, milestone, pr, release, repo, repo_collaborator, repo_deploy_key, search,
    tag, workflow_run, Command,
};

/// Should this command's output go through the pager?
pub(super) fn should_page(command: &Command) -> bool {
    match command {
        Command::Repo(cmd) => repo_pages(&cmd.command),
        Command::Issue(cmd) => issue_pages(&cmd.command),
        Command::Pr(cmd) => pr_pages(&cmd.command),
        Command::Release(cmd) => release_pages(&cmd.command),
        Command::Label(cmd) => matches!(&cmd.command, label::LabelSub::List(a) if !a.json),
        Command::Milestone(cmd) => {
            matches!(&cmd.command, milestone::MilestoneSub::List(a) if !a.json)
        }
        Command::Tag(cmd) => matches!(&cmd.command, tag::TagSub::List(a) if !a.json),
        Command::Search(cmd) => search_pages(&cmd.command),
        Command::Run(cmd) => run_pages(&cmd.command),
        Command::Api(args) => api_pages(args),
        _ => false,
    }
}

fn repo_pages(sub: &repo::RepoSub) -> bool {
    use repo::RepoSub as S;
    match sub {
        // Long read-only listings and the repo overview.
        S::List(a) => !a.json,
        S::View(a) => !a.json && !a.web,
        S::Starred(a) => !a.json,
        // `repo branches` lists when it has no action subcommand; with
        // `create` / `delete` it is a write.
        S::Branches(a) => a.command.is_none() && !a.json,
        S::Collaborator(c) => {
            matches!(&c.command, repo_collaborator::CollaboratorSub::List(a) if !a.json)
        }
        S::DeployKey(c) => {
            matches!(&c.command, repo_deploy_key::DeployKeySub::List(a) if !a.json)
        }
        // Writes, git-driven commands, and short one-off reads.
        S::Clone(_)
        | S::Create(_)
        | S::Fork(_)
        | S::Sync(_)
        | S::Edit(_)
        | S::Rename(_)
        | S::Archive(_)
        | S::Unarchive(_)
        | S::Delete(_)
        | S::Topics(_)
        | S::Mirror(_)
        | S::MirrorSync(_)
        | S::Watch(_)
        | S::Unwatch(_)
        | S::Star(_)
        | S::Unstar(_)
        | S::SetDefault(_) => false,
    }
}

fn issue_pages(sub: &issue::IssueSub) -> bool {
    use issue::IssueSub as S;
    match sub {
        S::List(a) => !a.json && !a.web,
        S::View(a) => !a.json && !a.web,
        S::Create(_)
        | S::Edit(_)
        | S::Close(_)
        | S::Reopen(_)
        | S::Lock(_)
        | S::Unlock(_)
        | S::Delete(_)
        | S::Pin(_)
        | S::Unpin(_)
        | S::Comment(_)
        | S::EditComment(_)
        | S::DeleteComment(_)
        | S::Develop(_) => false,
    }
}

fn pr_pages(sub: &pr::PrSub) -> bool {
    use pr::PrSub as S;
    match sub {
        S::List(a) => !a.json && !a.web,
        S::View(a) => !a.json && !a.web,
        // Diffs, commit lists, file lists, and check lists are read-only and
        // routinely longer than a screen.
        S::Diff(a) | S::Commits(a) | S::Files(a) | S::Checks(a) => !a.json,
        // `pr status` is a fixed three-section dashboard, not a long list.
        S::Status(_) => false,
        S::Create(_)
        | S::Edit(_)
        | S::Ready(_)
        | S::Comment(_)
        | S::Attach(_)
        | S::UpdateBranch(_)
        | S::Review(_)
        | S::RequestReview(_)
        | S::UnrequestReview(_)
        | S::Checkout(_)
        | S::Merge(_)
        | S::Close(_)
        | S::Reopen(_)
        | S::Lock(_)
        | S::Unlock(_) => false,
    }
}

fn release_pages(sub: &release::ReleaseSub) -> bool {
    use release::ReleaseSub as S;
    match sub {
        S::List(a) => !a.json && !a.web,
        S::View(a) => !a.json && !a.web,
        S::Create(_)
        | S::Edit(_)
        | S::Delete(_)
        | S::Upload(_)
        | S::Download(_)
        | S::DeleteAsset(_) => false,
    }
}

fn search_pages(sub: &search::SearchSub) -> bool {
    use search::SearchSub as S;
    match sub {
        S::Repos(a) | S::Issues(a) | S::Prs(a) | S::Users(a) => !a.json,
    }
}

fn run_pages(sub: &workflow_run::RunSub) -> bool {
    use workflow_run::RunSub as S;
    match sub {
        S::List(a) => !a.json,
        S::View(a) => !a.json,
        S::Logs(a) => !a.json,
        // `run watch` streams live; paging would buffer it until the job ends.
        S::Watch(_) => false,
        // Artifact listing is a handful of rows; downloading writes files and
        // reports progress.
        S::Download(_) => false,
        S::Rerun(_) | S::Cancel(_) => false,
    }
}

/// `fj api` GET dumps are the one machine-shaped output worth paging: they are
/// read-only and frequently thousands of lines. Mutating calls print a short
/// response (or prompt), and `--silent` prints nothing at all.
fn api_pages(args: &api::ApiArgs) -> bool {
    if args.silent {
        return false;
    }
    match api::pick_method(args) {
        Ok(m) => m == Method::GET || m == Method::HEAD,
        // A bad `-X` value fails in the handler; don't page the error.
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::should_page;
    use crate::cli::Cli;
    use clap::Parser;

    fn pages(argv: &[&str]) -> bool {
        let cli = Cli::try_parse_from(argv).expect("argv parses");
        should_page(&cli.command)
    }

    #[test]
    fn long_read_only_views_still_page() {
        assert!(pages(&["fj", "pr", "list"]));
        assert!(pages(&["fj", "pr", "view", "1"]));
        assert!(pages(&["fj", "pr", "diff", "1"]));
        assert!(pages(&["fj", "pr", "commits", "1"]));
        assert!(pages(&["fj", "pr", "files", "1"]));
        assert!(pages(&["fj", "pr", "checks", "1"]));
        assert!(pages(&["fj", "issue", "list"]));
        assert!(pages(&["fj", "issue", "view", "1"]));
        assert!(pages(&["fj", "repo", "list"]));
        assert!(pages(&["fj", "repo", "view"]));
        assert!(pages(&["fj", "repo", "starred"]));
        assert!(pages(&["fj", "repo", "branches"]));
        assert!(pages(&["fj", "repo", "collaborator", "list"]));
        assert!(pages(&["fj", "repo", "deploy-key", "list"]));
        assert!(pages(&["fj", "release", "list"]));
        assert!(pages(&["fj", "release", "view"]));
        assert!(pages(&["fj", "label", "list"]));
        assert!(pages(&["fj", "milestone", "list"]));
        assert!(pages(&["fj", "tag", "list"]));
        assert!(pages(&["fj", "search", "repos", "q"]));
        assert!(pages(&["fj", "search", "issues", "q"]));
        assert!(pages(&["fj", "search", "prs", "q"]));
        assert!(pages(&["fj", "search", "users", "q"]));
        assert!(pages(&["fj", "run", "list"]));
        assert!(pages(&["fj", "run", "view", "7"]));
        assert!(pages(&["fj", "run", "log", "7"]));
        assert!(pages(&["fj", "api", "/user"]));
    }

    #[test]
    fn status_and_summary_commands_do_not_page() {
        assert!(!pages(&["fj", "status"]));
        assert!(!pages(&["fj", "status", "--mark-read"]));
        assert!(!pages(&["fj", "pr", "status"]));
    }

    #[test]
    fn write_commands_do_not_page() {
        assert!(!pages(&[
            "fj", "pr", "create", "--title", "t", "--head", "h"
        ]));
        assert!(!pages(&["fj", "pr", "merge", "1"]));
        assert!(!pages(&["fj", "pr", "close", "1"]));
        assert!(!pages(&["fj", "pr", "reopen", "1"]));
        assert!(!pages(&["fj", "pr", "ready", "1"]));
        assert!(!pages(&["fj", "pr", "comment", "1", "-b", "hi"]));
        assert!(!pages(&["fj", "pr", "review", "1", "--event", "approve"]));
        assert!(!pages(&["fj", "pr", "checkout", "1"]));
        assert!(!pages(&["fj", "pr", "lock", "1"]));
        assert!(!pages(&["fj", "pr", "unlock", "1"]));
        assert!(!pages(&["fj", "issue", "create", "--title", "t"]));
        assert!(!pages(&["fj", "issue", "close", "1"]));
        assert!(!pages(&["fj", "issue", "comment", "1", "-b", "hi"]));
        assert!(!pages(&["fj", "repo", "create", "x"]));
        assert!(!pages(&["fj", "repo", "clone", "o/n"]));
        assert!(!pages(&["fj", "repo", "delete", "o/n"]));
        assert!(!pages(&["fj", "repo", "star", "o/n"]));
        assert!(!pages(&["fj", "repo", "fork"]));
        assert!(!pages(&["fj", "repo", "branches", "create", "b"]));
        assert!(!pages(&["fj", "repo", "collaborator", "add", "u"]));
        // `label create` / `label edit` are omitted on purpose: their
        // `--color` arg collides with the global `--color` id and makes clap
        // panic during parse. Pre-existing on main, unrelated to paging.
        assert!(!pages(&["fj", "label", "delete", "bug"]));
        assert!(!pages(&["fj", "milestone", "close", "1"]));
        assert!(!pages(&["fj", "tag", "create", "v1"]));
        assert!(!pages(&["fj", "tag", "delete", "v1"]));
        assert!(!pages(&["fj", "release", "create", "v1"]));
        assert!(!pages(&["fj", "release", "download"]));
        assert!(!pages(&["fj", "run", "cancel", "7"]));
        assert!(!pages(&["fj", "run", "rerun", "7"]));
        assert!(!pages(&["fj", "api", "-X", "POST", "/repos/o/n/issues"]));
        assert!(!pages(&["fj", "api", "-X", "delete", "/user/starred/o/n"]));
    }

    #[test]
    fn short_reads_do_not_page() {
        assert!(!pages(&["fj", "tag", "view", "v1"]));
        assert!(!pages(&["fj", "milestone", "view", "1"]));
        assert!(!pages(&["fj", "repo", "topics"]));
        assert!(!pages(&["fj", "repo", "set-default", "--view"]));
        assert!(!pages(&["fj", "run", "download", "7", "--list"]));
    }

    #[test]
    fn json_output_never_pages() {
        assert!(!pages(&["fj", "pr", "list", "--json"]));
        assert!(!pages(&["fj", "pr", "view", "1", "--json"]));
        assert!(!pages(&["fj", "pr", "diff", "1", "--json"]));
        assert!(!pages(&["fj", "issue", "list", "--json"]));
        assert!(!pages(&["fj", "issue", "view", "1", "--json"]));
        assert!(!pages(&["fj", "repo", "list", "--json"]));
        assert!(!pages(&["fj", "repo", "view", "--json"]));
        assert!(!pages(&["fj", "repo", "branches", "--json"]));
        assert!(!pages(&["fj", "repo", "starred", "--json"]));
        assert!(!pages(&["fj", "repo", "collaborator", "list", "--json"]));
        assert!(!pages(&["fj", "repo", "deploy-key", "list", "--json"]));
        assert!(!pages(&["fj", "release", "list", "--json"]));
        assert!(!pages(&["fj", "label", "list", "--json"]));
        assert!(!pages(&["fj", "milestone", "list", "--json"]));
        assert!(!pages(&["fj", "tag", "list", "--json"]));
        assert!(!pages(&["fj", "search", "repos", "q", "--json"]));
        assert!(!pages(&["fj", "run", "list", "--json"]));
        assert!(!pages(&["fj", "run", "log", "7", "--json"]));
        assert!(!pages(&["fj", "api", "/user", "--silent"]));
    }

    #[test]
    fn web_output_never_pages() {
        assert!(!pages(&["fj", "pr", "list", "--web"]));
        assert!(!pages(&["fj", "pr", "view", "1", "--web"]));
        assert!(!pages(&["fj", "issue", "list", "--web"]));
        assert!(!pages(&["fj", "issue", "view", "1", "--web"]));
        assert!(!pages(&["fj", "repo", "view", "--web"]));
        assert!(!pages(&["fj", "release", "list", "--web"]));
    }

    #[test]
    fn streaming_and_never_paged_families_stay_unpaged() {
        assert!(!pages(&["fj", "run", "watch", "7"]));
        assert!(!pages(&["fj", "auth", "status"]));
        assert!(!pages(&["fj", "config", "get", "editor"]));
        assert!(!pages(&["fj", "browse"]));
        assert!(!pages(&["fj", "workflow", "list"]));
        assert!(!pages(&["fj", "protect", "list"]));
        assert!(!pages(&["fj", "org", "list"]));
        assert!(!pages(&["fj", "hook", "list"]));
        assert!(!pages(&["fj", "alias", "list"]));
    }
}
