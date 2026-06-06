use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::context::resolve_repo;
use crate::cli::repo_deploy_key::DeployKeyCmd;
use crate::cli::repo_mirror::{MirrorArgs, MirrorSyncArgs};
use crate::cli::repo_social::{SimpleRepoArgs, StarredArgs};
use crate::cli::{repo_deploy_key, repo_mirror, repo_social};
use crate::client::Client;
use crate::config::hosts::Hosts;
use crate::git;
use crate::output;

use super::editor;
use super::web;

#[derive(Debug, Args)]
pub struct RepoCmd {
    #[command(subcommand)]
    pub command: RepoSub,
}

#[derive(Debug, Subcommand)]
pub enum RepoSub {
    /// List repositories you have access to on the current host.
    List(ListArgs),
    /// View a repository.
    View(ViewArgs),
    /// Clone a repository over git.
    Clone(CloneArgs),
    /// Create a new repository.
    Create(CreateArgs),
    /// Fork a repository.
    Fork(ForkArgs),
    /// Sync a fork with its upstream default branch.
    Sync(SyncArgs),
    /// Edit a repository's description, visibility, default branch.
    Edit(EditArgs),
    /// Rename a repository (in-place).
    Rename(RenameArgs),
    /// Archive a repository (read-only).
    Archive(ArchiveArgs),
    /// Unarchive a repository.
    Unarchive(ArchiveArgs),
    /// Delete a repository. Destructive.
    Delete(DeleteArgs),
    /// List branches.
    Branches(BranchesArgs),
    /// Manage repo topics (tags).
    Topics(TopicsArgs),
    /// Migrate or mirror a repo from another git host.
    Mirror(MirrorArgs),
    /// Manually trigger a sync on a pull-mirror.
    MirrorSync(MirrorSyncArgs),
    /// Watch a repo (subscribe to its notifications).
    Watch(SimpleRepoArgs),
    /// Stop watching a repo.
    Unwatch(SimpleRepoArgs),
    /// Star a repo.
    Star(SimpleRepoArgs),
    /// Unstar a repo.
    Unstar(SimpleRepoArgs),
    /// List your starred repos.
    Starred(StarredArgs),
    /// Pin which repo `fj` targets in this directory (like `gh repo set-default`).
    SetDefault(SetDefaultArgs),
    /// Manage repository deploy keys.
    DeployKey(DeployKeyCmd),
}

#[derive(Debug, Args)]
pub struct SetDefaultArgs {
    /// Repository as `<owner>/<name>` to pin for this directory.
    pub repo: Option<String>,
    /// Print the currently pinned default, if any.
    #[arg(long)]
    pub view: bool,
    /// Remove the pinned default.
    #[arg(long)]
    pub unset: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Maximum number of repositories to fetch
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    /// Page of results to fetch
    #[arg(long, default_value_t = 1)]
    pub page: u32,
    /// Search query (uses `/repos/search`).
    #[arg(short, long)]
    pub search: Option<String>,
    /// Emit raw JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    /// `owner/name` slug. Inferred from the git remote when omitted.
    pub repo: Option<String>,
    /// Alias for the positional repo arg.
    #[arg(short = 'R', long = "repo")]
    pub repo_flag: Option<String>,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
    /// Open the repo's web page.
    #[arg(long)]
    pub web: bool,
}

/// Choose between a positional repo arg and the `-R` flag.
pub(crate) fn pick_repo<'a>(pos: Option<&'a String>, flag: Option<&'a String>) -> Option<&'a str> {
    flag.map(|s| s.as_str()).or(pos.map(|s| s.as_str()))
}

#[derive(Debug, Args)]
pub struct CloneArgs {
    /// `owner/name` slug.
    pub repo: String,
    /// Optional destination directory.
    pub dir: Option<String>,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// `[owner/]name`. Without an owner, the authenticated user is used.
    pub repo: String,
    /// Repository description
    #[arg(short, long)]
    pub description: Option<String>,
    /// Make the repository private
    #[arg(long, default_value_t = false)]
    pub private: bool,
    /// Initialize with a commit
    #[arg(long, default_value_t = false)]
    pub init: bool,
    /// Clone the new repo into the current directory.
    #[arg(long)]
    pub clone: bool,
    /// Open the new repo in your browser.
    #[arg(long)]
    pub web: bool,
}

#[derive(Debug, Args)]
pub struct ForkArgs {
    /// Source repo `owner/name`. Inferred when omitted.
    pub repo: Option<String>,
    /// Alias for the positional repo arg
    #[arg(short = 'R', long = "repo")]
    pub repo_flag: Option<String>,
    /// Place the fork under this organization instead of your user account.
    #[arg(long)]
    pub org: Option<String>,
    /// New repository name.
    #[arg(long)]
    pub name: Option<String>,
    /// Clone the fork after creation.
    #[arg(long)]
    pub clone: bool,
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Repository as owner/name
    pub repo: Option<String>,
    /// Alias for the positional repo arg
    #[arg(short = 'R', long = "repo")]
    pub repo_flag: Option<String>,
    /// Branch to sync. Defaults to the repo's default branch.
    #[arg(long)]
    pub branch: Option<String>,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    /// Repository as owner/name
    pub repo: Option<String>,
    /// Alias for the positional repo arg
    #[arg(short = 'R', long = "repo")]
    pub repo_flag: Option<String>,
    /// Repository description
    #[arg(long)]
    pub description: Option<String>,
    /// Repository website URL
    #[arg(long)]
    pub website: Option<String>,
    /// Default branch name
    #[arg(long)]
    pub default_branch: Option<String>,
    /// Force private/public. Accepts `true` or `false`.
    #[arg(long)]
    pub private: Option<bool>,
    /// Open `$EDITOR` for the description.
    #[arg(long)]
    pub description_editor: bool,
}

#[derive(Debug, Args)]
pub struct RenameArgs {
    /// Repository as owner/name
    pub repo: String,
    /// New repository name
    pub new_name: String,
}

#[derive(Debug, Args)]
pub struct ArchiveArgs {
    /// Repository as owner/name
    pub repo: Option<String>,
    /// Alias for the positional repo arg
    #[arg(short = 'R', long = "repo")]
    pub repo_flag: Option<String>,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    /// Repository as owner/name
    pub repo: String,
    /// Skip the confirmation prompt.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct BranchesArgs {
    /// Repository as owner/name
    pub repo: Option<String>,
    /// Alias for the positional repo arg
    #[arg(short = 'R', long = "repo")]
    pub repo_flag: Option<String>,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct TopicsArgs {
    /// Repository as owner/name
    pub repo: Option<String>,
    /// Alias for the positional repo arg
    #[arg(short = 'R', long = "repo")]
    pub repo_flag: Option<String>,
    /// Set the topic list (comma-separated). Omit to just print.
    #[arg(long)]
    pub set: Option<String>,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: RepoCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        RepoSub::List(args) => list(args, host).await,
        RepoSub::View(args) => view(args, host).await,
        RepoSub::Clone(args) => clone(args, host).await,
        RepoSub::Create(args) => create(args, host).await,
        RepoSub::Fork(args) => fork(args, host).await,
        RepoSub::Sync(args) => sync(args, host).await,
        RepoSub::Edit(args) => edit(args, host).await,
        RepoSub::Rename(args) => rename(args, host).await,
        RepoSub::Archive(args) => set_archived(args, host, true).await,
        RepoSub::Unarchive(args) => set_archived(args, host, false).await,
        RepoSub::Delete(args) => delete(args, host).await,
        RepoSub::Branches(args) => branches(args, host).await,
        RepoSub::Topics(args) => topics(args, host).await,
        RepoSub::Mirror(args) => repo_mirror::mirror(args, host).await,
        RepoSub::MirrorSync(args) => repo_mirror::mirror_sync(args, host).await,
        RepoSub::Watch(args) => repo_social::watch(args, host, true).await,
        RepoSub::Unwatch(args) => repo_social::watch(args, host, false).await,
        RepoSub::Star(args) => repo_social::star(args, host, true).await,
        RepoSub::Unstar(args) => repo_social::star(args, host, false).await,
        RepoSub::Starred(args) => repo_social::starred(args, host).await,
        RepoSub::SetDefault(args) => set_default(args).await,
        RepoSub::DeployKey(cmd) => repo_deploy_key::deploy_key(cmd, host).await,
    }
}

/// `fj repo set-default` pins the repo this directory's commands target,
/// stored as `fj.resolved` in local git config and read back by `resolve_repo`.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn set_default(args: SetDefaultArgs) -> Result<()> {
    const KEY: &str = "fj.resolved";
    if args.view {
        match git::config_get_local(KEY) {
            Some(v) => println!("{v}"),
            None => println!("(no default set for this directory)"),
        }
        return Ok(());
    }
    if args.unset {
        git::config_unset_local(KEY)?;
        println!("✓ Cleared the pinned default repo");
        return Ok(());
    }
    let slug = args
        .repo
        .ok_or_else(|| anyhow!("pass a repo as `<owner>/<name>`, or use --view / --unset"))?;
    // Validate the shape before writing it.
    let _ = api::split_repo(&slug)?;
    git::config_set_local(KEY, &slug)?;
    println!("✓ Default repo for this directory set to {slug}");
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    let opts = api::repo::ListOptions {
        limit: args.limit,
        page: args.page,
        query: args.search.as_deref(),
    };
    let page = if args.search.is_some() {
        api::repo::search(&client, opts).await?
    } else {
        api::repo::list_for_user(&client, opts).await?
    };
    if args.json {
        let v = serde_json::to_value(&page.items)?;
        return output::print_json(&v);
    }
    if page.items.is_empty() {
        println!("(no repositories)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = page
        .items
        .iter()
        .map(|r| {
            let visibility = if r.private { "private" } else { "public" };
            vec![
                r.full_name.clone(),
                output::truncate(&r.description, 60),
                visibility.into(),
                output::dim(&output::relative_time(r.updated_at)),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["NAME", "DESCRIPTION", "VISIBILITY", "UPDATED"], &rows)
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn view(args: ViewArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(pick_repo(args.repo.as_ref(), args.repo_flag.as_ref()), host)?;
    let repo = api::repo::get(&ctx.client, &ctx.owner, &ctx.name).await?;
    if args.web {
        return web::open(&repo.html_url);
    }
    if args.json {
        return output::print_json(&serde_json::to_value(&repo)?);
    }
    let header = output::bold(&repo.full_name);
    let vis = if repo.private { "private" } else { "public" };
    let extra = if repo.archived {
        format!(" {}", output::dim("(archived)"))
    } else {
        String::new()
    };
    println!("{header}  {}{extra}", output::dim(vis));
    if !repo.description.is_empty() {
        println!("{}", repo.description);
    }
    println!();
    println!("Default branch:  {}", repo.default_branch);
    println!("Stars:           {}", repo.stars_count);
    println!("Forks:           {}", repo.forks_count);
    println!("Open issues:     {}", repo.open_issues_count);
    println!(
        "Updated:         {}",
        output::relative_time(repo.updated_at)
    );
    println!();
    println!("URL:        {}", repo.html_url);
    println!("Clone URL:  {}", repo.clone_url);
    println!("SSH URL:    {}", repo.ssh_url);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn clone(args: CloneArgs, host: Option<&str>) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    let client = Client::connect(host)?;
    let repo = api::repo::get(&client, owner, name).await?;
    let hosts = Hosts::load()?;
    let hostname = hosts.resolve_host(host)?;
    let proto = hosts
        .hosts
        .get(hostname)
        .map(|h| h.git_protocol.clone())
        .unwrap_or_else(|| "https".into());
    let url = if proto == "ssh" {
        repo.ssh_url
    } else {
        repo.clone_url
    };
    git::clone(&url, args.dir.as_deref())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn create(args: CreateArgs, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    let (owner, name) = match args.repo.matches('/').count() {
        0 => (None, args.repo.clone()),
        1 => {
            let (o, n) = api::split_repo(&args.repo)?;
            (Some(o.to_string()), n.to_string())
        }
        _ => return Err(anyhow!("expected '[owner/]name', got '{}'", args.repo)),
    };
    let body = api::repo::CreateRepo {
        name: &name,
        description: args.description.as_deref(),
        private: args.private,
        default_branch: None,
        auto_init: args.init,
    };
    let repo = match owner {
        Some(o) => match api::repo::create_for_org(&client, &o, &body).await {
            Ok(r) => r,
            Err(_) => api::repo::create_for_current_user(&client, &body).await?,
        },
        None => api::repo::create_for_current_user(&client, &body).await?,
    };
    println!("✓ Created {}", repo.full_name);
    println!("{}", repo.html_url);
    if args.clone {
        let hosts = Hosts::load()?;
        let hostname = hosts.resolve_host(host)?;
        let proto = hosts
            .hosts
            .get(hostname)
            .map(|h| h.git_protocol.clone())
            .unwrap_or_else(|| "https".into());
        let url = if proto == "ssh" {
            repo.ssh_url.clone()
        } else {
            repo.clone_url.clone()
        };
        git::clone(&url, None)?;
    }
    if args.web {
        web::open(&repo.html_url)?;
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn fork(args: ForkArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(pick_repo(args.repo.as_ref(), args.repo_flag.as_ref()), host)?;
    let r = api::repo::fork(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.org.as_deref(),
        args.name.as_deref(),
    )
    .await?;
    println!("✓ Forked to {}", r.full_name);
    println!("{}", r.html_url);
    if args.clone {
        let hosts = Hosts::load()?;
        let hostname = hosts.resolve_host(host)?;
        let proto = hosts
            .hosts
            .get(hostname)
            .map(|h| h.git_protocol.clone())
            .unwrap_or_else(|| "https".into());
        let url = if proto == "ssh" {
            r.ssh_url
        } else {
            r.clone_url
        };
        git::clone(&url, None)?;
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn sync(args: SyncArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(pick_repo(args.repo.as_ref(), args.repo_flag.as_ref()), host)?;
    let branch = match args.branch {
        Some(b) => b,
        None => {
            api::repo::get(&ctx.client, &ctx.owner, &ctx.name)
                .await?
                .default_branch
        }
    };
    api::repo::sync_with_upstream(&ctx.client, &ctx.owner, &ctx.name, &branch).await?;
    println!(
        "✓ Synced {}/{} branch {} with upstream",
        ctx.owner, ctx.name, branch
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn edit(args: EditArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(pick_repo(args.repo.as_ref(), args.repo_flag.as_ref()), host)?;
    let description = if args.description_editor {
        let existing = api::repo::get(&ctx.client, &ctx.owner, &ctx.name).await?;
        Some(editor::edit_text(
            "REPO_DESCRIPTION.md",
            &existing.description,
        )?)
    } else {
        args.description
    };
    let body = api::repo::EditRepo {
        description: description.as_deref(),
        website: args.website.as_deref(),
        private: args.private,
        default_branch: args.default_branch.as_deref(),
        archived: None,
    };
    let r = api::repo::edit(&ctx.client, &ctx.owner, &ctx.name, &body).await?;
    println!("✓ Updated {}", r.full_name);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn rename(args: RenameArgs, host: Option<&str>) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    let client = Client::connect(host)?;
    api::repo::rename(&client, owner, name, &args.new_name).await?;
    println!("✓ Renamed {}/{} → {}/{}", owner, name, owner, args.new_name);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn set_archived(args: ArchiveArgs, host: Option<&str>, archived: bool) -> Result<()> {
    let ctx = resolve_repo(pick_repo(args.repo.as_ref(), args.repo_flag.as_ref()), host)?;
    let body = api::repo::EditRepo {
        archived: Some(archived),
        ..Default::default()
    };
    api::repo::edit(&ctx.client, &ctx.owner, &ctx.name, &body).await?;
    println!(
        "✓ {} {}/{}",
        if archived { "Archived" } else { "Unarchived" },
        ctx.owner,
        ctx.name
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn delete(args: DeleteArgs, host: Option<&str>) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    let client = Client::connect(host)?;
    if !args.yes {
        let prompt = format!("Delete {owner}/{name}? Type the slug to confirm");
        let answer = editor::prompt_line(&prompt)?;
        if answer != args.repo {
            return Err(anyhow!("aborted: slug did not match"));
        }
    }
    api::repo::delete(&client, owner, name).await?;
    println!("✓ Deleted {owner}/{name}");
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn branches(args: BranchesArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(pick_repo(args.repo.as_ref(), args.repo_flag.as_ref()), host)?;
    let bs = api::repo::list_branches(&ctx.client, &ctx.owner, &ctx.name).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&bs)?);
    }
    if bs.is_empty() {
        println!("(no branches)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = bs
        .iter()
        .map(|b| {
            vec![
                b.name.clone(),
                output::dim(&b.commit.id[..7.min(b.commit.id.len())]),
                output::truncate(b.commit.message.lines().next().unwrap_or(""), 60),
                if b.protected {
                    output::bold("protected")
                } else {
                    String::new()
                },
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["BRANCH", "SHA", "SUBJECT", ""], &rows)
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn topics(args: TopicsArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(pick_repo(args.repo.as_ref(), args.repo_flag.as_ref()), host)?;
    if let Some(set) = args.set {
        let list: Vec<String> = set
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        api::repo::set_topics(&ctx.client, &ctx.owner, &ctx.name, &list).await?;
        println!("✓ Topics set to: {}", list.join(", "));
        return Ok(());
    }
    let topics = api::repo::list_topics(&ctx.client, &ctx.owner, &ctx.name).await?;
    if topics.is_empty() {
        println!("(no topics)");
    } else {
        for t in topics {
            println!("{t}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_repo_prefers_the_flag_over_positional() {
        let pos = String::from("owner/pos");
        let flag = String::from("owner/flag");
        assert_eq!(pick_repo(Some(&pos), Some(&flag)), Some("owner/flag"));
    }

    #[test]
    fn pick_repo_falls_back_to_positional() {
        let pos = String::from("owner/pos");
        assert_eq!(pick_repo(Some(&pos), None), Some("owner/pos"));
    }

    #[test]
    fn pick_repo_none_when_neither() {
        assert_eq!(pick_repo(None, None), None);
    }
}
