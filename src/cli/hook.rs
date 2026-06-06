//! `fj hook` — repository webhooks.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api::hook::{self, CreateHook};
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::cli::editor;
use crate::output;

#[derive(Debug, Args)]
pub struct HookCmd {
    #[command(subcommand)]
    pub command: HookSub,
}

#[derive(Debug, Subcommand)]
pub enum HookSub {
    /// List webhooks configured on a repo.
    List(ListArgs),
    /// Create a new webhook pointing at a URL.
    Create(CreateArgs),
    /// Delete a webhook by id.
    Delete(DeleteArgs),
    /// Send a test delivery to an existing webhook.
    Test(HookIdArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Webhook URL.
    pub url: String,
    /// Hook type. Defaults to `gitea` (Gitea-compatible JSON payload).
    #[arg(long, default_value = "gitea")]
    pub r#type: String,
    /// Events to subscribe to (comma-separated). Defaults to `push`.
    #[arg(long, default_value = "push")]
    pub events: String,
    /// Set Content-Type for the request.
    #[arg(long, default_value = "json")]
    pub content_type: String,
    /// HMAC secret used to sign webhook deliveries.
    #[arg(long)]
    pub secret: Option<String>,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Webhook id
    pub id: u64,
    /// Skip the confirmation prompt.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct HookIdArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Webhook id
    pub id: u64,
}

pub async fn run(cmd: HookCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        HookSub::List(args) => list(args, host).await,
        HookSub::Create(args) => create(args, host).await,
        HookSub::Delete(args) => delete(args, host).await,
        HookSub::Test(args) => test(args, host).await,
    }
}

async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let items = hook::list(&ctx.client, &ctx.owner, &ctx.name).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&items)?);
    }
    if items.is_empty() {
        println!("(no webhooks)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|h| {
            let url = h
                .config
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            vec![
                h.id.to_string(),
                h.type_.clone(),
                url,
                h.events.join(","),
                h.active.to_string(),
                output::dim(&output::relative_time(h.updated_at)),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["ID", "TYPE", "URL", "EVENTS", "ACTIVE", "UPDATED"], &rows)
    );
    Ok(())
}

async fn create(args: CreateArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let events: Vec<&str> = args
        .events
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let mut config = serde_json::Map::new();
    config.insert("url".into(), serde_json::Value::String(args.url.clone()));
    config.insert(
        "content_type".into(),
        serde_json::Value::String(args.content_type),
    );
    if let Some(s) = &args.secret {
        config.insert("secret".into(), serde_json::Value::String(s.clone()));
    }
    let body = CreateHook {
        type_: &args.r#type,
        config: serde_json::Value::Object(config),
        events,
        active: true,
    };
    let h = hook::create(&ctx.client, &ctx.owner, &ctx.name, &body).await?;
    println!("✓ Created webhook #{} → {}", h.id, args.url);
    Ok(())
}

async fn delete(args: DeleteArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if !args.yes {
        let ans = editor::prompt_line(&format!(
            "Delete webhook #{} from {}? Type the id to confirm",
            args.id,
            ctx.slug()
        ))?;
        if ans.trim() != args.id.to_string() {
            println!("aborted");
            return Ok(());
        }
    }
    hook::delete(&ctx.client, &ctx.owner, &ctx.name, args.id).await?;
    println!("✓ Deleted webhook #{}", args.id);
    Ok(())
}

async fn test(args: HookIdArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    hook::test(&ctx.client, &ctx.owner, &ctx.name, args.id).await?;
    println!("✓ Triggered test delivery for webhook #{}", args.id);
    Ok(())
}
