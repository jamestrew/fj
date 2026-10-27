use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::api::release::{Asset, CreateRelease, EditRelease, ListOptions, Release};
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
    /// Release tag (defaults to the latest published release)
    pub tag: Option<String>,
    /// View the latest published release. Redundant when TAG is omitted.
    #[arg(long)]
    pub latest: bool,
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
    /// Read release notes from file (use `-` for stdin).
    #[arg(short = 'F', long = "body-file", value_name = "FILE")]
    pub body_file: Option<String>,
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
    /// Replace an existing asset with the same name before uploading
    #[arg(long)]
    pub clobber: bool,
}

#[derive(Debug, Args)]
pub struct DownloadArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Release tag (defaults to the latest published release)
    pub tag: Option<String>,
    /// Download from the latest published release. Redundant when TAG is omitted.
    #[arg(long)]
    pub latest: bool,
    /// Asset name. If omitted, all assets are downloaded.
    #[arg(short = 'n', long)]
    pub name: Option<String>,
    /// Asset glob pattern. Repeat to match any pattern.
    #[arg(short = 'p', long = "pattern", value_name = "GLOB")]
    pub patterns: Vec<String>,
    /// Output directory (default: cwd).
    #[arg(short = 'o', long)]
    pub output_dir: Option<String>,
    /// Overwrite existing files instead of skipping them.
    #[arg(long)]
    pub clobber: bool,
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
    let r = resolve_release_for_tag_arg(&ctx, args.tag.as_deref()).await?;
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
    let body = editor::resolve_body_input(
        args.body.as_deref(),
        args.body_file.as_deref(),
        Some(("RELEASE_BODY.md", "")),
    )?;
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
        upload_one_for_release(&ctx, &release, path, args.clobber).await?;
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn upload_one_for_release(
    ctx: &crate::cli::context::RepoContext,
    release: &Release,
    path: &str,
    clobber: bool,
) -> Result<()> {
    let name = asset_name_for_path(path);
    let existing: Vec<&Asset> = release.assets.iter().filter(|a| a.name == name).collect();
    if !existing.is_empty() && !clobber {
        return Err(anyhow!(
            "release {} already has asset '{}' (use --clobber to replace it)",
            release.tag_name,
            name
        ));
    }
    for asset in existing {
        api::release::delete_asset(&ctx.client, &ctx.owner, &ctx.name, release.id, asset.id)
            .await?;
    }
    upload_one_with_name(ctx, release.id, path, &name).await
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn upload_one(
    ctx: &crate::cli::context::RepoContext,
    release_id: u64,
    path: &str,
) -> Result<()> {
    let name = asset_name_for_path(path);
    upload_one_with_name(ctx, release_id, path, &name).await
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn upload_one_with_name(
    ctx: &crate::cli::context::RepoContext,
    release_id: u64,
    path: &str,
    name: &str,
) -> Result<()> {
    let bytes = std::fs::read(path)?;
    let asset =
        api::release::upload_asset(&ctx.client, &ctx.owner, &ctx.name, release_id, name, bytes)
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
    let release = resolve_release_for_tag_arg(&ctx, args.tag.as_deref()).await?;
    let patterns = compile_patterns(&args.patterns)?;
    let out = args.output_dir.unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&out)?;
    if release.assets.is_empty() {
        return Err(anyhow!("release {} has no assets", release.tag_name));
    }
    let filters = AssetFilters {
        name: args.name.as_deref(),
        patterns: &patterns,
    };
    let filter_label = filters.label();
    let tag = release.tag_name.clone();
    let mut downloaded = 0usize;
    let mut skipped = 0usize;
    let mut matched = 0usize;
    for a in &release.assets {
        if !filters.matches(&a.name) {
            continue;
        }
        matched += 1;
        let dest = safe_asset_dest(Path::new(&out), &a.name)?;
        if dest.exists() && !args.clobber {
            println!(
                "• Skipped {} ({} exists; use --clobber to overwrite)",
                a.name,
                dest.display()
            );
            skipped += 1;
            continue;
        }
        let bytes = crate::download::verified_body(
            || async {
                Ok(ctx
                    .client
                    .http()
                    .get(&a.browser_download_url)
                    .send()
                    .await?)
            },
            Some(a.size),
            a.name.to_ascii_lowercase().ends_with(".zip"),
            "release asset",
        )
        .await?;
        crate::download::write_atomic(&dest, &bytes)?;
        println!("✓ Downloaded {} → {}", a.name, dest.display());
        downloaded += 1;
    }
    if matched == 0 {
        return Err(anyhow!(
            "release {tag} has no assets matching {}",
            filter_label
        ));
    }
    if downloaded == 0 && skipped > 0 {
        println!("(all matching assets already exist)");
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn resolve_release_for_tag_arg(
    ctx: &crate::cli::context::RepoContext,
    tag: Option<&str>,
) -> Result<Release> {
    if let Some(tag) = tag {
        return api::release::get_by_tag(&ctx.client, &ctx.owner, &ctx.name, tag).await;
    }
    latest_published_release(ctx).await
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn latest_published_release(ctx: &crate::cli::context::RepoContext) -> Result<Release> {
    let page = api::release::list(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        ListOptions {
            limit: 30,
            page: 1,
            paginate: true,
        },
    )
    .await?;
    page.items
        .into_iter()
        .find(|r| r.published_at.is_some() && !r.draft && !r.prerelease)
        .ok_or_else(|| {
            anyhow!(
                "repository {}/{} has no latest published release",
                ctx.owner,
                ctx.name
            )
        })
}

fn asset_name_for_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

struct AssetFilters<'a> {
    name: Option<&'a str>,
    patterns: &'a [AssetPattern],
}

impl AssetFilters<'_> {
    fn matches(&self, asset_name: &str) -> bool {
        match (self.name, self.patterns.is_empty()) {
            (None, true) => true,
            (Some(name), _) if name == asset_name => true,
            _ => self
                .patterns
                .iter()
                .any(|pattern| pattern.matches(asset_name)),
        }
    }

    fn label(&self) -> String {
        let mut filters = Vec::new();
        if let Some(name) = self.name {
            filters.push(format!("name '{name}'"));
        }
        filters.extend(
            self.patterns
                .iter()
                .map(|pattern| format!("pattern '{}'", pattern.raw)),
        );
        if filters.is_empty() {
            "any asset".into()
        } else {
            filters.join(" or ")
        }
    }
}

fn compile_patterns(patterns: &[String]) -> Result<Vec<AssetPattern>> {
    patterns
        .iter()
        .map(|pattern| AssetPattern::parse(pattern))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AssetPattern {
    raw: String,
    tokens: Vec<GlobToken>,
}

impl AssetPattern {
    fn parse(raw: &str) -> Result<Self> {
        let mut chars = raw.chars().peekable();
        let mut tokens = Vec::new();
        while let Some(c) = chars.next() {
            match c {
                '*' => tokens.push(GlobToken::Star),
                '?' => tokens.push(GlobToken::Any),
                '[' => {
                    let mut negated = false;
                    if matches!(chars.peek(), Some('!')) {
                        chars.next();
                        negated = true;
                    }
                    let mut class = Vec::new();
                    loop {
                        match chars.next() {
                            Some(']') if !class.is_empty() => break,
                            Some(ch) => class.push(ch),
                            None => return Err(anyhow!("invalid --pattern {raw:?}: missing ']'")),
                        }
                    }
                    tokens.push(GlobToken::Class {
                        chars: class,
                        negated,
                    });
                }
                '\\' => {
                    let ch = chars
                        .next()
                        .ok_or_else(|| anyhow!("invalid --pattern {raw:?}: trailing '\\\\'"))?;
                    tokens.push(GlobToken::Literal(ch));
                }
                ch => tokens.push(GlobToken::Literal(ch)),
            }
        }
        Ok(Self {
            raw: raw.to_string(),
            tokens,
        })
    }

    fn matches(&self, value: &str) -> bool {
        let chars: Vec<char> = value.chars().collect();
        let mut dp = vec![vec![false; chars.len() + 1]; self.tokens.len() + 1];
        dp[0][0] = true;
        for i in 0..self.tokens.len() {
            for j in 0..=chars.len() {
                if !dp[i][j] {
                    continue;
                }
                match &self.tokens[i] {
                    GlobToken::Star => {
                        dp[i + 1][j] = true;
                        if j < chars.len() {
                            dp[i][j + 1] = true;
                        }
                    }
                    token if j < chars.len() && token.matches(chars[j]) => {
                        dp[i + 1][j + 1] = true;
                    }
                    _ => {}
                }
            }
        }
        dp[self.tokens.len()][chars.len()]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GlobToken {
    Star,
    Any,
    Literal(char),
    Class { chars: Vec<char>, negated: bool },
}

impl GlobToken {
    fn matches(&self, value: char) -> bool {
        match self {
            Self::Star => true,
            Self::Any => true,
            Self::Literal(ch) => *ch == value,
            Self::Class { chars, negated } => chars.contains(&value) != *negated,
        }
    }
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
    use super::{compile_patterns, safe_asset_dest, AssetFilters};
    use crate::cli::{Cli, Command};
    use clap::Parser;
    use std::path::Path;

    #[test]
    fn release_view_tag_is_optional() {
        let cli = Cli::try_parse_from(["fj", "release", "view", "--json", "-R", "o/r"])
            .expect("should parse without tag");
        match cli.command {
            Command::Release(cmd) => match cmd.command {
                super::ReleaseSub::View(args) => {
                    assert_eq!(args.tag, None);
                    assert!(args.json);
                }
                other => panic!("expected release view, got {other:?}"),
            },
            other => panic!("expected release command, got {other:?}"),
        }
    }

    #[test]
    fn release_latest_is_allowed_without_tag() {
        let cli = Cli::try_parse_from(["fj", "release", "view", "--latest", "-R", "o/r"])
            .expect("--latest without tag should parse");
        match cli.command {
            Command::Release(cmd) => match cmd.command {
                super::ReleaseSub::View(args) => {
                    assert_eq!(args.tag, None);
                    assert!(args.latest);
                }
                other => panic!("expected release view, got {other:?}"),
            },
            other => panic!("expected release command, got {other:?}"),
        }
    }

    #[test]
    fn release_download_latest_is_allowed_without_tag() {
        let cli = Cli::try_parse_from(["fj", "release", "download", "--latest", "-R", "o/r"])
            .expect("download --latest without tag should parse");
        match cli.command {
            Command::Release(cmd) => match cmd.command {
                super::ReleaseSub::Download(args) => {
                    assert_eq!(args.tag, None);
                    assert!(args.latest);
                }
                other => panic!("expected release download, got {other:?}"),
            },
            other => panic!("expected release command, got {other:?}"),
        }
    }

    #[test]
    fn release_download_parses_patterns_and_clobber_without_tag() {
        let cli = Cli::try_parse_from([
            "fj",
            "release",
            "download",
            "--pattern",
            "*.tar.gz",
            "--pattern",
            "*.zip",
            "--clobber",
            "-R",
            "o/r",
        ])
        .expect("should parse repeatable patterns without tag");
        match cli.command {
            Command::Release(cmd) => match cmd.command {
                super::ReleaseSub::Download(args) => {
                    assert_eq!(args.tag, None);
                    assert_eq!(args.patterns, ["*.tar.gz", "*.zip"]);
                    assert!(args.clobber);
                }
                other => panic!("expected release download, got {other:?}"),
            },
            other => panic!("expected release command, got {other:?}"),
        }
    }

    #[test]
    fn release_upload_parses_clobber() {
        let cli = Cli::try_parse_from([
            "fj",
            "release",
            "upload",
            "v1.2.3",
            "dist/fj.tar.gz",
            "--clobber",
            "-R",
            "o/r",
        ])
        .expect("should parse upload clobber");
        match cli.command {
            Command::Release(cmd) => match cmd.command {
                super::ReleaseSub::Upload(args) => {
                    assert_eq!(args.tag, "v1.2.3");
                    assert_eq!(args.files, ["dist/fj.tar.gz"]);
                    assert!(args.clobber);
                }
                other => panic!("expected release upload, got {other:?}"),
            },
            other => panic!("expected release command, got {other:?}"),
        }
    }

    #[test]
    fn asset_filters_match_exact_name_or_any_pattern() {
        let patterns = compile_patterns(&["*.tar.gz".into(), "fj-*-linux-?.zip".into()])
            .expect("patterns should compile");
        let filters = AssetFilters {
            name: Some("SHA256SUMS"),
            patterns: &patterns,
        };
        assert!(filters.matches("SHA256SUMS"));
        assert!(filters.matches("fj-v1.2.3-linux-x.zip"));
        assert!(filters.matches("fj-v1.2.3-linux-x86_64.tar.gz"));
        assert!(!filters.matches("fj-v1.2.3-darwin-x86_64.tar.xz"));
    }

    #[test]
    fn asset_pattern_reports_invalid_glob() {
        let err = compile_patterns(&["fj-[linux".into()]).unwrap_err();
        assert!(err.to_string().contains("missing ']'"));
    }

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
