//! `fj repo mirror` / `mirror-sync` — migrate or mirror a repo from another
//! git host, and trigger a pull-mirror sync.

use anyhow::{anyhow, Result};
use clap::Args;

use crate::api;
use crate::cli::context::resolve_repo;
use crate::cli::repo::pick_repo;
use crate::client::Client;

#[derive(Debug, Args)]
pub struct MirrorArgs {
    /// Source URL (e.g. `https://github.com/foo/bar.git`).
    pub source_url: String,
    /// Destination as `[owner/]name`. Defaults to the source's basename
    /// under your user account.
    #[arg(long)]
    pub dest: Option<String>,
    /// Migrate as a pull-mirror instead of a one-shot import.
    #[arg(long)]
    pub mirror: bool,
    /// Make the destination repo private.
    #[arg(long)]
    pub private: bool,
    /// Description for the new repo.
    #[arg(long)]
    pub description: Option<String>,
    /// Username for HTTP source-auth.
    #[arg(long)]
    pub auth_user: Option<String>,
    /// Password / token for HTTP source-auth.
    #[arg(long)]
    pub auth_pass: Option<String>,
    /// Service hint: git, github, gitea, gitlab, gogs.
    #[arg(long, default_value = "git")]
    pub service: String,
    /// Refresh interval for pull-mirrors (e.g. `8h`, `1d`).
    #[arg(long)]
    pub interval: Option<String>,
}

#[derive(Debug, Args)]
pub struct MirrorSyncArgs {
    /// Pull-mirror repository as owner/name (inferred from the git remote when omitted)
    pub repo: Option<String>,
    /// Alias for the positional repository arg
    #[arg(short = 'R', long = "repo")]
    pub repo_flag: Option<String>,
}

pub(crate) async fn mirror(args: MirrorArgs, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    let me = api::user::current(&client).await?;
    let (dest_owner, dest_name) = match args.dest.as_deref() {
        Some(slug) if slug.matches('/').count() == 0 => (me.login.clone(), slug.to_string()),
        Some(slug) if slug.matches('/').count() == 1 => {
            let (o, n) = api::split_repo(slug)?;
            (o.to_string(), n.to_string())
        }
        Some(slug) => return Err(anyhow!("expected '[owner/]name', got '{slug}'")),
        None => {
            let basename = std::path::Path::new(&args.source_url)
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| anyhow!("can't infer destination name from {}", args.source_url))?;
            (me.login.clone(), basename.to_string())
        }
    };
    let body = api::repo::Migrate {
        clone_addr: &args.source_url,
        repo_name: &dest_name,
        repo_owner: &dest_owner,
        mirror: args.mirror,
        private: args.private,
        description: args.description.as_deref(),
        auth_username: args.auth_user.as_deref(),
        auth_password: args.auth_pass.as_deref(),
        auth_token: None,
        service: &args.service,
        mirror_interval: args.interval.as_deref(),
    };
    let r = api::repo::migrate(&client, &body).await?;
    println!(
        "✓ {} {}",
        if args.mirror { "Mirrored" } else { "Imported" },
        r.full_name
    );
    println!("{}", r.html_url);
    Ok(())
}

pub(crate) async fn mirror_sync(args: MirrorSyncArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(pick_repo(args.repo.as_ref(), args.repo_flag.as_ref()), host)?;
    api::repo::mirror_sync(&ctx.client, &ctx.owner, &ctx.name).await?;
    println!("✓ Triggered mirror sync for {}/{}", ctx.owner, ctx.name);
    Ok(())
}
