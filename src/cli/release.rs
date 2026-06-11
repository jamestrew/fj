use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::api::release::{CreateRelease, EditRelease, ListOptions};
use crate::cli::context::{repo_web_url, resolve_repo, RepoFlag};
use crate::cli::list as list_output;
use crate::output;

use super::editor;
use super::web;

#[derive(Debug, Args)]
pub struct ReleaseCmd {
    #[command(subcommand)]
    pub command: ReleaseSub,
}

#[derive(Debug, Subcommand)]
pub enum ReleaseSub {
    /// List releases on a repository.
    List(ListArgs),
    /// View a release by tag.
    View(ViewArgs),
    /// Create a release.
    Create(CreateArgs),
    /// Edit a release.
    Edit(EditArgs),
    /// Delete a release.
    Delete(DeleteArgs),
    /// Upload an asset file to a release.
    Upload(UploadArgs),
    /// Download a release asset by name.
    Download(DownloadArgs),
    /// Delete a single asset from a release by name.
    DeleteAsset(DeleteAssetArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Maximum number of releases to fetch (default 30; results may be capped unless --paginate)
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    /// Fetch all pages instead of stopping at --limit
    #[arg(long)]
    pub paginate: bool,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
    /// Open the release in your browser
    #[arg(long)]
    pub web: bool,
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Release tag
    pub tag: String,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
    /// Open the release in your browser
    #[arg(long)]
    pub web: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Release tag
    pub tag: String,
    /// Release title
    #[arg(short = 't', long)]
    pub title: Option<String>,
    /// Release notes (use - for stdin, omit to open $EDITOR)
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Target commitish (branch or SHA). Defaults to default branch.
    #[arg(long)]
    pub target: Option<String>,
    /// Save as a draft instead of publishing
    #[arg(long)]
    pub draft: bool,
    /// Mark as a prerelease
    #[arg(long)]
    pub prerelease: bool,
    /// Files to upload as assets (repeatable).
    #[arg(long = "asset", value_name = "FILE")]
    pub assets: Vec<String>,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Release tag
    pub tag: String,
    /// Release title
    #[arg(short = 't', long)]
    pub title: Option<String>,
    /// Release notes (use - for stdin, omit to open $EDITOR)
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Save as a draft instead of publishing
    #[arg(long)]
    pub draft: Option<bool>,
    /// Mark as a prerelease
    #[arg(long)]
    pub prerelease: Option<bool>,
    /// Edit the release notes in $EDITOR
    #[arg(long)]
    pub body_editor: bool,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Release tag
    pub tag: String,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct UploadArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Release tag
    pub tag: String,
    /// File paths to upload.
    pub files: Vec<String>,
}

#[derive(Debug, Args)]
pub struct DownloadArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Release tag
    pub tag: String,
    /// Asset name. If omitted, all assets are downloaded.
    #[arg(short = 'n', long)]
    pub name: Option<String>,
    /// Output directory (default: cwd).
    #[arg(short = 'o', long)]
    pub output_dir: Option<String>,
}

#[derive(Debug, Args)]
pub struct DeleteAssetArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Release tag
    pub tag: String,
    /// Name of the asset to delete.
    pub asset: String,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: ReleaseCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        ReleaseSub::List(args) => list(args, host).await,
        ReleaseSub::View(args) => view(args, host).await,
        ReleaseSub::Create(args) => create(args, host).await,
        ReleaseSub::Edit(args) => edit(args, host).await,
        ReleaseSub::Delete(args) => delete(args, host).await,
        ReleaseSub::Upload(args) => upload(args, host).await,
        ReleaseSub::Download(args) => download(args, host).await,
        ReleaseSub::DeleteAsset(args) => delete_asset(args, host).await,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if args.web {
        return web::open(&repo_web_url(&ctx, "releases")?);
    }
    let page = api::release::list(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        ListOptions {
            limit: args.limit,
            page: 1,
            paginate: args.paginate,
        },
    )
    .await?;
    if args.json {
        return list_output::print_json(&page);
    }
    if page.items.is_empty() {
        println!("(no releases)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = page
        .items
        .iter()
        .map(|r| {
            let when = r
                .published_at
                .map(output::relative_time)
                .unwrap_or_else(|| "draft".into());
            let kind = if r.prerelease {
                "pre-release"
            } else if r.draft {
                "draft"
            } else {
                "release"
            };
            vec![
                r.tag_name.clone(),
                output::truncate(&r.name, 60),
                output::dim(kind),
                output::dim(&when),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["TAG", "TITLE", "KIND", "PUBLISHED"], &rows)
    );
    if !args.paginate {
        list_output::print_truncation_footer(&page, args.limit);
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn view(args: ViewArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let r = api::release::get_by_tag(&ctx.client, &ctx.owner, &ctx.name, &args.tag).await?;
    if args.web {
        return web::open(&r.html_url);
    }
    if args.json {
        return output::print_json(&serde_json::to_value(&r)?);
    }
    println!(
        "{}  {}",
        output::bold(&format!("{}  {}", r.tag_name, r.name)),
        output::dim(&r.html_url),
    );
    if r.prerelease {
        println!("(prerelease)");
    }
    if r.draft {
        println!("(draft)");
    }
    println!();
    println!("Target: {}", r.target_commitish);
    let when = r
        .published_at
        .map(output::relative_time)
        .unwrap_or_else(|| "—".into());
    println!("Published: {when}");
    println!();
    if !r.body.is_empty() {
        println!("{}", r.body);
        println!();
    }
    if !r.assets.is_empty() {
        println!("{}", output::bold("Assets:"));
        for a in &r.assets {
            println!(
                "  • {}  {}  {}",
                a.name,
                output::dim(&output::human_size(a.size)),
                output::dim(&a.browser_download_url)
            );
        }
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn create(args: CreateArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let body = editor::resolve_body(args.body.as_deref(), "RELEASE_BODY.md", "")?;
    let payload = CreateRelease {
        tag_name: &args.tag,
        name: args.title.as_deref(),
        body: body.as_deref(),
        target_commitish: args.target.as_deref(),
        draft: args.draft,
        prerelease: args.prerelease,
    };
    let release = api::release::create(&ctx.client, &ctx.owner, &ctx.name, &payload).await?;
    println!("✓ Created release {}", release.tag_name);
    println!("{}", release.html_url);

    for path in &args.assets {
        upload_one(&ctx, release.id, path).await?;
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn edit(args: EditArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let existing = api::release::get_by_tag(&ctx.client, &ctx.owner, &ctx.name, &args.tag).await?;
    let body = if args.body_editor {
        Some(editor::edit_text("RELEASE_BODY.md", &existing.body)?)
    } else {
        args.body
    };
    let payload = EditRelease {
        tag_name: None,
        name: args.title.as_deref(),
        body: body.as_deref(),
        draft: args.draft,
        prerelease: args.prerelease,
    };
    let r = api::release::edit(&ctx.client, &ctx.owner, &ctx.name, existing.id, &payload).await?;
    println!("✓ Updated release {}", r.tag_name);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn delete(args: DeleteArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let existing = api::release::get_by_tag(&ctx.client, &ctx.owner, &ctx.name, &args.tag).await?;
    if !args.yes {
        let answer =
            editor::prompt_line(&format!("Delete release {}? Type tag to confirm", args.tag))?;
        if answer != args.tag {
            return Err(anyhow::anyhow!("aborted"));
        }
    }
    api::release::delete(&ctx.client, &ctx.owner, &ctx.name, existing.id).await?;
    println!("✓ Deleted release {}", args.tag);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn delete_asset(args: DeleteAssetArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let release = api::release::get_by_tag(&ctx.client, &ctx.owner, &ctx.name, &args.tag).await?;
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == args.asset)
        .ok_or_else(|| {
            anyhow::anyhow!("release {} has no asset named '{}'", args.tag, args.asset)
        })?;
    if !args.yes {
        let answer = editor::prompt_line(&format!(
            "Delete asset '{}' from release {}? Type the asset name to confirm",
            args.asset, args.tag
        ))?;
        if answer != args.asset {
            return Err(anyhow::anyhow!("aborted"));
        }
    }
    api::release::delete_asset(&ctx.client, &ctx.owner, &ctx.name, release.id, asset.id).await?;
    println!("✓ Deleted asset {} from {}", args.asset, args.tag);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn upload(args: UploadArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let release = api::release::get_by_tag(&ctx.client, &ctx.owner, &ctx.name, &args.tag).await?;
    for path in &args.files {
        upload_one(&ctx, release.id, path).await?;
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn upload_one(
    ctx: &crate::cli::context::RepoContext,
    release_id: u64,
    path: &str,
) -> Result<()> {
    let bytes = std::fs::read(path)?;
    let name = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string();
    let asset =
        api::release::upload_asset(&ctx.client, &ctx.owner, &ctx.name, release_id, &name, bytes)
            .await?;
    println!(
        "✓ Uploaded {} ({})",
        asset.name,
        output::human_size(asset.size)
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn download(args: DownloadArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let release = api::release::get_by_tag(&ctx.client, &ctx.owner, &ctx.name, &args.tag).await?;
    let out = args.output_dir.unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&out)?;
    let mut downloaded = 0usize;
    for a in &release.assets {
        if let Some(filter) = &args.name {
            if &a.name != filter {
                continue;
            }
        }
        let dest = safe_asset_dest(Path::new(&out), &a.name)?;
        let resp = ctx
            .client
            .http()
            .get(&a.browser_download_url)
            .send()
            .await?
            .error_for_status()?;
        crate::client::check_body_size(&resp)?;
        let bytes = resp.bytes().await?;
        std::fs::write(&dest, &bytes)?;
        println!("✓ Downloaded {} → {}", a.name, dest.display());
        downloaded += 1;
    }
    if downloaded == 0 {
        println!("(no assets matched)");
    }
    Ok(())
}

/// Resolve the on-disk destination for a release asset. The asset name is
/// returned by the server and is therefore untrusted: it must be a single
/// normal path component (no separators, no `.`/`..`, not absolute). We then
/// re-check that the joined path stays directly inside `out`. Without this a
/// malicious repo could ship an asset named `../../../../etc/whatever` and
/// `Path::join` would happily write outside the download directory.
fn safe_asset_dest(out: &Path, asset_name: &str) -> Result<PathBuf> {
    let mut comps = Path::new(asset_name).components();
    let only = match (comps.next(), comps.next()) {
        (Some(std::path::Component::Normal(c)), None) => c,
        _ => {
            return Err(anyhow!(
                "refusing to write release asset with unsafe name {asset_name:?}"
            ))
        }
    };
    if only.is_empty() {
        return Err(anyhow!("release asset has an empty name"));
    }
    let dest = out.join(only);
    if dest.parent() != Some(out) {
        return Err(anyhow!(
            "refusing to write release asset outside {}: {asset_name:?}",
            out.display()
        ));
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::safe_asset_dest;
    use std::path::Path;

    #[test]
    fn safe_asset_dest_accepts_plain_name() {
        let dest = safe_asset_dest(Path::new("/tmp/out"), "fj-linux.tar.gz").unwrap();
        assert_eq!(dest, Path::new("/tmp/out/fj-linux.tar.gz"));
    }

    #[test]
    fn safe_asset_dest_rejects_traversal() {
        assert!(safe_asset_dest(Path::new("/tmp/out"), "../evil").is_err());
        assert!(safe_asset_dest(Path::new("/tmp/out"), "../../etc/passwd").is_err());
        assert!(safe_asset_dest(Path::new("/tmp/out"), "a/b").is_err());
        assert!(safe_asset_dest(Path::new("/tmp/out"), "/etc/passwd").is_err());
        assert!(safe_asset_dest(Path::new("/tmp/out"), "..").is_err());
        assert!(safe_asset_dest(Path::new("/tmp/out"), "").is_err());
        assert!(safe_asset_dest(Path::new("/tmp/out"), ".").is_err());
    }
}
