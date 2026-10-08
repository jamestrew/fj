use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api::label::{self, CreateLabel, EditLabel};
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::cli::label_org::{self, Owner};
use crate::output;

#[derive(Debug, Args)]
pub struct LabelCmd {
    #[command(subcommand)]
    pub command: LabelSub,
}

#[derive(Debug, Subcommand)]
pub enum LabelSub {
    /// List a repo's labels, or an organization's with `--org`. The two are
    /// separate collections: a repo list never includes organization labels.
    List(ListArgs),
    /// Create a new label in a repo, or in an organization with `--org`.
    Create(CreateArgs),
    /// Edit a label's name, color, description, or exclusive flag.
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
    /// List the organization's own labels (shared by its repositories)
    /// instead of a repo's.
    #[arg(long, value_name = "ORG", conflicts_with = "repo")]
    pub org: Option<String>,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Create the label on the organization (shared by its repositories)
    /// instead of on a repo.
    #[arg(long, value_name = "ORG", conflicts_with = "repo")]
    pub org: Option<String>,
    /// Label name. With `--exclusive`, use the form `group/name`.
    pub name: String,
    /// Color as 6-char hex (with or without `#`). Named `--label-color`
    /// because the global `--color` flag controls ANSI output.
    #[arg(
        long = "label-color",
        id = "label_color",
        value_name = "HEX",
        default_value = "ededed"
    )]
    pub color: String,
    /// Label description
    #[arg(long)]
    pub description: Option<String>,
    /// Make this an exclusive label: an issue keeps only one label from the
    /// `group/` part of the name, and adding this one replaces another.
    #[arg(long)]
    pub exclusive: bool,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Edit the organization's label of this name instead of a repo's.
    #[arg(long, value_name = "ORG", conflicts_with = "repo")]
    pub org: Option<String>,
    /// Label name to edit.
    pub name: String,
    /// New name.
    #[arg(long)]
    pub rename: Option<String>,
    /// New color as 6-char hex (e.g. d73a4a). Named `--label-color` because
    /// the global `--color` flag controls ANSI output.
    #[arg(long = "label-color", id = "label_color", value_name = "HEX")]
    pub color: Option<String>,
    /// Label description
    #[arg(long)]
    pub description: Option<String>,
    /// Make the label exclusive within the `group/` part of its name.
    #[arg(long, conflicts_with = "no_exclusive")]
    pub exclusive: bool,
    /// Make the label an ordinary, non-exclusive label.
    #[arg(long)]
    pub no_exclusive: bool,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Delete the organization's label of this name instead of a repo's.
    #[arg(long, value_name = "ORG", conflicts_with = "repo")]
    pub org: Option<String>,
    /// Label name
    pub name: String,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: LabelCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        LabelSub::List(args) => list(args, host).await,
        LabelSub::Create(args) => create(args, host).await,
        LabelSub::Edit(args) => edit(args, host).await,
        LabelSub::Delete(args) => delete(args, host).await,
        LabelSub::Clone(args) => clone_labels(args, host).await,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
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
                        // The exclusive flag is part of the label: a forced
                        // copy carries it either way.
                        exclusive: Some(l.exclusive),
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
                        exclusive: l.exclusive.then_some(true),
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

#[cfg_attr(coverage_nightly, coverage(off))]
async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    if let Some(org) = args.org.as_deref() {
        return label_org::list(org, args.json, host).await;
    }
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let items = label::list(&ctx.client, &ctx.owner, &ctx.name).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&items)?);
    }
    if items.is_empty() {
        println!("(no labels)");
        return Ok(());
    }
    print!(
        "{}",
        output::render_table(&label_org::LIST_HEADERS, &label_org::label_rows(&items))
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn create(args: CreateArgs, host: Option<&str>) -> Result<()> {
    if let Some(org) = args.org.as_deref() {
        return label_org::create(
            org,
            &args.name,
            &args.color,
            args.description.as_deref(),
            args.exclusive,
            host,
        )
        .await;
    }
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let color = label_org::normalize_color(&args.color);
    let l = label::create(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        &CreateLabel {
            name: &args.name,
            color: &color,
            description: args.description.as_deref(),
            exclusive: args.exclusive.then_some(true),
        },
    )
    .await?;
    println!(
        "{}",
        label_org::saved_message("Created", Owner::Repo(&ctx.owner, &ctx.name), &l)
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn edit(args: EditArgs, host: Option<&str>) -> Result<()> {
    let color = args.color.as_deref().map(label_org::normalize_color);
    let patch = EditLabel {
        name: args.rename.as_deref(),
        color: color.as_deref(),
        description: args.description.as_deref(),
        exclusive: label_org::exclusive_flag(args.exclusive, args.no_exclusive),
    };
    if let Some(org) = args.org.as_deref() {
        return label_org::edit(org, &args.name, &patch, host).await;
    }
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let existing = label::get_by_name(&ctx.client, &ctx.owner, &ctx.name, &args.name).await?;
    let l = label::edit(&ctx.client, &ctx.owner, &ctx.name, existing.id, &patch).await?;
    println!(
        "{}",
        label_org::saved_message("Updated", Owner::Repo(&ctx.owner, &ctx.name), &l)
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn delete(args: DeleteArgs, host: Option<&str>) -> Result<()> {
    if let Some(org) = args.org.as_deref() {
        return label_org::delete(org, &args.name, args.yes, host).await;
    }
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let existing = label::get_by_name(&ctx.client, &ctx.owner, &ctx.name, &args.name).await?;
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
