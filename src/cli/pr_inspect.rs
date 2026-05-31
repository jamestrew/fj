//! Read-only inspection subcommands for `fj pr`: diff, commits, files, and
//! CI check status. Each operates on a single PR identified by `SimpleArgs`
//! and only reads, never mutates.

use anyhow::Result;

use crate::api;
use crate::cli::context::resolve_repo;
use crate::cli::pr::SimpleArgs;
use crate::output;

pub async fn diff(args: SimpleArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let text = api::pull::diff_text(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    print!("{text}");
    Ok(())
}

pub async fn commits(args: SimpleArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let cs = api::pull::commits(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    let rows: Vec<Vec<String>> = cs
        .iter()
        .map(|c| {
            let short = c.sha.get(..7).unwrap_or(&c.sha).to_string();
            let subject = c.commit.message.lines().next().unwrap_or("").to_string();
            let author = c
                .commit
                .author
                .as_ref()
                .map(|a| a.name.clone())
                .unwrap_or_default();
            vec![short, output::truncate(&subject, 70), output::dim(&author)]
        })
        .collect();
    if rows.is_empty() {
        println!("(no commits)");
    } else {
        print!(
            "{}",
            output::render_table(&["SHA", "SUBJECT", "AUTHOR"], &rows)
        );
    }
    Ok(())
}

pub async fn files(args: SimpleArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let fs = api::pull::files(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    let rows: Vec<Vec<String>> = fs
        .iter()
        .map(|f| {
            vec![
                f.status.clone(),
                f.filename.clone(),
                output::dim(&format!("+{} -{}", f.additions, f.deletions)),
            ]
        })
        .collect();
    if rows.is_empty() {
        println!("(no file changes)");
    } else {
        print!(
            "{}",
            output::render_table(&["STATUS", "FILE", "DIFF"], &rows)
        );
    }
    Ok(())
}

pub async fn checks(args: SimpleArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let pr = api::pull::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    let cs = api::pull::combined_status(&ctx.client, &ctx.owner, &ctx.name, &pr.head.sha).await?;
    println!(
        "{}  {}",
        output::bold(&format!(
            "Combined: {}",
            if cs.state.is_empty() {
                "—"
            } else {
                &cs.state
            }
        )),
        output::dim(&format!(
            "{} checks on {}",
            cs.total_count,
            &pr.head.sha[..7.min(pr.head.sha.len())]
        )),
    );
    if cs.statuses.is_empty() {
        println!("(no per-check statuses reported)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = cs
        .statuses
        .iter()
        .map(|s| {
            let when = s
                .updated_at
                .map(output::relative_time)
                .unwrap_or_else(|| "—".into());
            vec![
                output::state_pill(&s.status.to_lowercase(), false),
                s.context.clone(),
                output::truncate(&s.description, 50),
                output::dim(&when),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["STATE", "CHECK", "DESCRIPTION", "WHEN"], &rows)
    );
    Ok(())
}
