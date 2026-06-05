//! `fj milestone` — Forgejo milestones (releases-by-target-date grouping).

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api;
use crate::api::milestone::{CreateMilestone, EditMilestone};
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::output;

use super::editor;

#[derive(Debug, Args)]
pub struct MilestoneCmd {
    #[command(subcommand)]
    pub command: MilestoneSub,
}

#[derive(Debug, Subcommand)]
pub enum MilestoneSub {
    /// List milestones in a repo.
    List(ListArgs),
    /// View a milestone by id.
    View(IdArgs),
    /// Create a milestone.
    Create(CreateArgs),
    /// Edit a milestone.
    Edit(EditArgs),
    /// Close a milestone.
    Close(IdArgs),
    /// Reopen a closed milestone.
    Reopen(IdArgs),
    /// Delete a milestone.
    Delete(DeleteArgs),
    /// Assign an issue or PR to a milestone (or detach with --milestone none).
    Assign(AssignArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Filter by state
    #[arg(short = 's', long, default_value = "open")]
    pub state: String,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct IdArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Milestone id
    pub id: u64,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Milestone title
    pub title: String,
    /// Description (use - for stdin, omit to open $EDITOR)
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Due date in ISO-8601 (e.g. 2026-06-01T00:00:00Z).
    #[arg(long)]
    pub due_on: Option<String>,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Milestone id
    pub id: u64,
    /// Milestone title
    #[arg(short = 't', long)]
    pub title: Option<String>,
    /// Description (use - for stdin, omit to open $EDITOR)
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Due date (YYYY-MM-DD)
    #[arg(long)]
    pub due_on: Option<String>,
    /// Edit the description in $EDITOR
    #[arg(long)]
    pub body_editor: bool,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Milestone id
    pub id: u64,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct AssignArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Issue or PR number.
    pub issue: u64,
    /// Milestone id, or `none` to detach.
    #[arg(long)]
    pub milestone: String,
}

pub async fn run(cmd: MilestoneCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        MilestoneSub::List(args) => list(args, host).await,
        MilestoneSub::View(args) => view(args, host).await,
        MilestoneSub::Create(args) => create(args, host).await,
        MilestoneSub::Edit(args) => edit(args, host).await,
        MilestoneSub::Close(args) => set_state(args, host, "closed").await,
        MilestoneSub::Reopen(args) => set_state(args, host, "open").await,
        MilestoneSub::Delete(args) => delete(args, host).await,
        MilestoneSub::Assign(args) => assign(args, host).await,
    }
}

async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let items = api::milestone::list(&ctx.client, &ctx.owner, &ctx.name, &args.state).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&items)?);
    }
    if items.is_empty() {
        println!("(no milestones)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|m| {
            let due = m
                .due_on
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            vec![
                m.id.to_string(),
                m.title.clone(),
                m.state.clone(),
                format!("{}/{}", m.closed_issues, m.open_issues + m.closed_issues),
                due,
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["ID", "TITLE", "STATE", "DONE", "DUE"], &rows)
    );
    Ok(())
}

async fn view(args: IdArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let m = api::milestone::get(&ctx.client, &ctx.owner, &ctx.name, args.id).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&m)?);
    }
    let total = m.open_issues + m.closed_issues;
    let pct = (m.closed_issues.checked_mul(100))
        .and_then(|n| n.checked_div(total))
        .unwrap_or(0);
    println!("{}", output::bold(&format!("{} ({})", m.title, m.state)));
    println!(
        "Progress: {}/{} ({}%)",
        m.closed_issues,
        m.open_issues + m.closed_issues,
        pct
    );
    if let Some(d) = m.due_on {
        println!("Due:      {}", d.format("%Y-%m-%d"));
    }
    if !m.description.is_empty() {
        println!();
        println!("{}", m.description);
    }
    Ok(())
}

async fn create(args: CreateArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let body = editor::read_body(args.body.as_deref())?;
    let m = api::milestone::create(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        &CreateMilestone {
            title: &args.title,
            description: body.as_deref(),
            due_on: args.due_on.as_deref(),
        },
    )
    .await?;
    println!("✓ Created milestone #{}: {}", m.id, m.title);
    Ok(())
}

async fn edit(args: EditArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let body = if args.body_editor {
        let existing = api::milestone::get(&ctx.client, &ctx.owner, &ctx.name, args.id).await?;
        Some(editor::edit_text(
            "MILESTONE_DESCRIPTION.md",
            &existing.description,
        )?)
    } else {
        args.body
    };
    let m = api::milestone::edit(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.id,
        &EditMilestone {
            title: args.title.as_deref(),
            description: body.as_deref(),
            state: None,
            due_on: args.due_on.as_deref(),
        },
    )
    .await?;
    println!("✓ Updated milestone #{}: {}", m.id, m.title);
    Ok(())
}

async fn set_state(args: IdArgs, host: Option<&str>, state: &str) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let m = api::milestone::edit(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.id,
        &EditMilestone {
            state: Some(state),
            ..Default::default()
        },
    )
    .await?;
    println!("✓ Milestone #{} is now {}", m.id, m.state);
    Ok(())
}

async fn delete(args: DeleteArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if !args.yes {
        let ans = editor::prompt_line(&format!(
            "Delete milestone {}? Type the id to confirm",
            args.id
        ))?;
        if ans != args.id.to_string() {
            return Err(anyhow::anyhow!("aborted"));
        }
    }
    api::milestone::delete(&ctx.client, &ctx.owner, &ctx.name, args.id).await?;
    println!("✓ Deleted milestone {}", args.id);
    Ok(())
}

async fn assign(args: AssignArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let milestone_id = if args.milestone.eq_ignore_ascii_case("none") {
        None
    } else {
        Some(
            args.milestone
                .parse::<u64>()
                .map_err(|_| anyhow::anyhow!("--milestone must be a number or 'none'"))?,
        )
    };
    api::milestone::assign(&ctx.client, &ctx.owner, &ctx.name, args.issue, milestone_id).await?;
    match milestone_id {
        Some(id) => println!("✓ Assigned #{} to milestone {}", args.issue, id),
        None => println!("✓ Detached #{} from any milestone", args.issue),
    }
    Ok(())
}
