use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api::tag::{self, CreateTag};
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::output;

use super::editor;

#[derive(Debug, Args)]
pub struct TagCmd {
    #[command(subcommand)]
    pub command: TagSub,
}

#[derive(Debug, Subcommand)]
pub enum TagSub {
    /// List git tags in a repo.
    List(ListArgs),
    /// View a git tag.
    View(ViewArgs),
    /// Create a git tag.
    Create(CreateArgs),
    /// Delete a git tag.
    Delete(DeleteArgs),
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
pub struct ViewArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Tag name
    pub tag: String,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Tag name
    pub tag: String,
    /// Commit SHA or ref to tag. Defaults to the server's default branch.
    #[arg(long)]
    pub target: Option<String>,
    /// Annotated tag message. Omit for a lightweight tag.
    #[arg(short = 'm', long)]
    pub message: Option<String>,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Tag name
    pub tag: String,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

pub async fn run(cmd: TagCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        TagSub::List(args) => list(args, host).await,
        TagSub::View(args) => view(args, host).await,
        TagSub::Create(args) => create(args, host).await,
        TagSub::Delete(args) => delete(args, host).await,
    }
}

async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let tags = tag::list(&ctx.client, &ctx.owner, &ctx.name).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&tags)?);
    }
    if tags.is_empty() {
        println!("(no tags)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = tags
        .iter()
        .map(|t| {
            vec![
                t.name.clone(),
                short_sha(&t.commit.sha),
                tag_message_summary(&t.message),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["TAG", "SHA", "MESSAGE"], &rows)
    );
    Ok(())
}

async fn view(args: ViewArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let t = tag::get(&ctx.client, &ctx.owner, &ctx.name, &args.tag).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&t)?);
    }
    println!("{}", output::bold(&t.name));
    if !t.commit.sha.is_empty() {
        println!("Commit:  {}", t.commit.sha);
    }
    if !t.message.is_empty() {
        println!("Message: {}", t.message);
    }
    if !t.tarball_url.is_empty() {
        println!("Tarball: {}", t.tarball_url);
    }
    if !t.zipball_url.is_empty() {
        println!("Zipball: {}", t.zipball_url);
    }
    Ok(())
}

async fn create(args: CreateArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let t = tag::create(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        &CreateTag {
            tag_name: &args.tag,
            target: args.target.as_deref(),
            message: args.message.as_deref(),
        },
    )
    .await?;
    println!("✓ Created tag '{}'", t.name);
    Ok(())
}

async fn delete(args: DeleteArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if !args.yes {
        let answer = editor::prompt_line(&format!(
            "Delete tag '{}'? Type the name to confirm",
            args.tag
        ))?;
        if answer != args.tag {
            return Err(anyhow::anyhow!("aborted"));
        }
    }
    tag::delete(&ctx.client, &ctx.owner, &ctx.name, &args.tag).await?;
    println!("✓ Deleted tag '{}'", args.tag);
    Ok(())
}

fn tag_message_summary(message: &str) -> String {
    output::truncate(message.lines().next().unwrap_or(""), 60)
}

fn short_sha(sha: &str) -> String {
    if sha.is_empty() {
        String::new()
    } else {
        output::dim(&sha[..7.min(sha.len())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_message_summary_uses_first_line_and_truncates() {
        let summary = tag_message_summary(&format!("{}\nsecond line", "x".repeat(80)));
        assert!(!summary.contains("second line"));
        assert!(summary.chars().count() <= 60);
    }
}
