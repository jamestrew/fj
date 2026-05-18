use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::api::release::{CreateRelease, EditRelease};
use crate::cli::context::{resolve_repo, RepoFlag};
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
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub web: bool,
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub tag: String,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub web: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub tag: String,
    #[arg(short = 't', long)]
    pub title: Option<String>,
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Target commitish (branch or SHA). Defaults to default branch.
    #[arg(long)]
    pub target: Option<String>,
    #[arg(long)]
    pub draft: bool,
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
    pub tag: String,
    #[arg(short = 't', long)]
    pub title: Option<String>,
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    #[arg(long)]
    pub draft: Option<bool>,
    #[arg(long)]
    pub prerelease: Option<bool>,
    #[arg(long)]
    pub body_editor: bool,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub tag: String,
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct UploadArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub tag: String,
    /// File paths to upload.
    pub files: Vec<String>,
}

#[derive(Debug, Args)]
pub struct DownloadArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub tag: String,
    /// Asset name. If omitted, all assets are downloaded.
    #[arg(short = 'n', long)]
    pub name: Option<String>,
    /// Output directory (default: cwd).
    #[arg(short = 'o', long)]
    pub output_dir: Option<String>,
}

pub async fn run(cmd: ReleaseCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        ReleaseSub::List(args) => list(args, host).await,
        ReleaseSub::View(args) => view(args, host).await,
        ReleaseSub::Create(args) => create(args, host).await,
        ReleaseSub::Edit(args) => edit(args, host).await,
        ReleaseSub::Delete(args) => delete(args, host).await,
        ReleaseSub::Upload(args) => upload(args, host).await,
        ReleaseSub::Download(args) => download(args, host).await,
    }
}

async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if args.web {
        return web::open(&format!(
            "https://{}/{}/{}/releases",
            ctx.host, ctx.owner, ctx.name
        ));
    }
    let items = api::release::list(&ctx.client, &ctx.owner, &ctx.name, args.limit).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&items)?);
    }
    if items.is_empty() {
        println!("(no releases)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = items
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
                truncate(&r.name, 60),
                output::dim(kind),
                output::dim(&when),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["TAG", "TITLE", "KIND", "PUBLISHED"], &rows)
    );
    Ok(())
}

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
                output::dim(&human_size(a.size)),
                output::dim(&a.browser_download_url)
            );
        }
    }
    Ok(())
}

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

async fn upload(args: UploadArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let release = api::release::get_by_tag(&ctx.client, &ctx.owner, &ctx.name, &args.tag).await?;
    for path in &args.files {
        upload_one(&ctx, release.id, path).await?;
    }
    Ok(())
}

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
    println!("✓ Uploaded {} ({})", asset.name, human_size(asset.size));
    Ok(())
}

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

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{}B", bytes)
    } else {
        format!("{:.1}{}", size, UNITS[unit])
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::{human_size, safe_asset_dest};
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

    #[test]
    fn formats_bytes() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(512), "512B");
    }

    #[test]
    fn formats_kb() {
        assert_eq!(human_size(2048), "2.0KB");
    }

    #[test]
    fn formats_mb_gb() {
        assert!(human_size(5 * 1024 * 1024).ends_with("MB"));
        assert!(human_size(2 * 1024_u64.pow(3)).ends_with("GB"));
    }
}
