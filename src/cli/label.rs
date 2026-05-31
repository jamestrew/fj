use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api::label::{self, CreateLabel, EditLabel};
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::output;

#[derive(Debug, Args)]
pub struct LabelCmd {
    #[command(subcommand)]
    pub command: LabelSub,
}

#[derive(Debug, Subcommand)]
pub enum LabelSub {
    /// List labels in a repo.
    List(ListArgs),
    /// Create a new label.
    Create(CreateArgs),
    /// Edit a label's name, color, or description.
    Edit(EditArgs),
    /// Delete a label.
    Delete(DeleteArgs),
    /// Copy labels from another repo into this one.
    Clone(CloneArgs),
}

#[derive(Debug, Args)]
pub struct CloneArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Source repo as `<owner>/<name>` to copy labels from.
    pub source: String,
    /// Overwrite color/description of labels that already exist here.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub name: String,
    /// Color as 6-char hex (with or without `#`).
    #[arg(long, default_value = "ededed")]
    pub color: String,
    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Label name to edit.
    pub name: String,
    /// New name.
    #[arg(long)]
    pub rename: Option<String>,
    #[arg(long)]
    pub color: Option<String>,
    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub name: String,
    #[arg(short = 'y', long)]
    pub yes: bool,
}

pub async fn run(cmd: LabelCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        LabelSub::List(args) => list(args, host).await,
        LabelSub::Create(args) => create(args, host).await,
        LabelSub::Edit(args) => edit(args, host).await,
        LabelSub::Delete(args) => delete(args, host).await,
        LabelSub::Clone(args) => clone_labels(args, host).await,
    }
}

async fn clone_labels(args: CloneArgs, host: Option<&str>) -> Result<()> {
    let dest = resolve_repo(args.r.repo.as_deref(), host)?;
    let (so, sn) = crate::api::split_repo(&args.source)?;
    let source = label::list(&dest.client, so, sn).await?;
    if source.is_empty() {
        println!("(source repo has no labels)");
        return Ok(());
    }
    // name -> id for labels already in the destination.
    let existing: std::collections::HashMap<String, u64> =
        label::list(&dest.client, &dest.owner, &dest.name)
            .await?
            .into_iter()
            .map(|l| (l.name, l.id))
            .collect();
    let (mut created, mut updated, mut skipped) = (0u32, 0u32, 0u32);
    for l in &source {
        let desc = if l.description.is_empty() {
            None
        } else {
            Some(l.description.as_str())
        };
        match existing.get(&l.name) {
            Some(_) if !args.force => skipped += 1,
            Some(&id) => {
                label::edit(
                    &dest.client,
                    &dest.owner,
                    &dest.name,
                    id,
                    &EditLabel {
                        name: None,
                        color: Some(&l.color),
                        description: desc,
                    },
                )
                .await?;
                updated += 1;
            }
            None => {
                label::create(
                    &dest.client,
                    &dest.owner,
                    &dest.name,
                    &CreateLabel {
                        name: &l.name,
                        color: &l.color,
                        description: desc,
                    },
                )
                .await?;
                created += 1;
            }
        }
    }
    println!(
        "✓ Cloned labels from {}: {created} created, {updated} updated, {skipped} skipped",
        args.source
    );
    Ok(())
}

async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let items = label::list(&ctx.client, &ctx.owner, &ctx.name).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&items)?);
    }
    if items.is_empty() {
        println!("(no labels)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|l| {
            vec![
                l.name.clone(),
                format!("#{}", l.color),
                truncate(&l.description, 60),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["NAME", "COLOR", "DESCRIPTION"], &rows)
    );
    Ok(())
}

async fn create(args: CreateArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let color = args.color.trim_start_matches('#').to_string();
    let l = label::create(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        &CreateLabel {
            name: &args.name,
            color: &color,
            description: args.description.as_deref(),
        },
    )
    .await?;
    println!("✓ Created label '{}' (#{})", l.name, l.color);
    Ok(())
}

async fn edit(args: EditArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let existing = label::list(&ctx.client, &ctx.owner, &ctx.name)
        .await?
        .into_iter()
        .find(|l| l.name == args.name)
        .ok_or_else(|| anyhow::anyhow!("no label named '{}'", args.name))?;
    let color = args
        .color
        .as_ref()
        .map(|c| c.trim_start_matches('#').to_string());
    let l = label::edit(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        existing.id,
        &EditLabel {
            name: args.rename.as_deref(),
            color: color.as_deref(),
            description: args.description.as_deref(),
        },
    )
    .await?;
    println!("✓ Updated label '{}'", l.name);
    Ok(())
}

async fn delete(args: DeleteArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let existing = label::list(&ctx.client, &ctx.owner, &ctx.name)
        .await?
        .into_iter()
        .find(|l| l.name == args.name)
        .ok_or_else(|| anyhow::anyhow!("no label named '{}'", args.name))?;
    if !args.yes {
        let answer = crate::cli::editor::prompt_line(&format!(
            "Delete label '{}'? Type the name to confirm",
            args.name
        ))?;
        if answer != args.name {
            return Err(anyhow::anyhow!("aborted"));
        }
    }
    label::delete(&ctx.client, &ctx.owner, &ctx.name, existing.id).await?;
    println!("✓ Deleted label '{}'", args.name);
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
