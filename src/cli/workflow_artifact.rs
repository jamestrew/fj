//! `fj run download` — list or download a workflow run's artifacts. Split out
//! from the run/log handlers in `workflow_run` since artifacts are a separate
//! Forgejo surface (their own endpoints and on-disk output handling).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use clap::Args;

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::cli::workflow_run::guard_web_routes;
use crate::output;

#[derive(Debug, Args)]
pub struct DownloadArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Run number, as shown in the `#` column of `fj run list`.
    pub run_id: u64,
    /// Download only this artifact (by name). Omit to download all.
    #[arg(short = 'n', long)]
    pub name: Option<String>,
    /// Output directory (default: cwd). Each artifact is written as `<name>.zip`.
    #[arg(short = 'o', long)]
    pub output_dir: Option<String>,
    /// List the run's artifacts instead of downloading.
    #[arg(long)]
    pub list: bool,
    #[arg(long)]
    pub json: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn download(args: DownloadArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    guard_web_routes(&ctx.client)?;
    let arts =
        api::workflow::list_artifacts(&ctx.client, &ctx.owner, &ctx.name, args.run_id).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&arts)?);
    }
    if arts.is_empty() {
        println!("(no artifacts)");
        return Ok(());
    }
    if args.list {
        let rows: Vec<Vec<String>> = arts
            .iter()
            .map(|a| {
                vec![
                    a.name.clone(),
                    output::human_size(a.size),
                    output::action_status(&a.status),
                ]
            })
            .collect();
        print!(
            "{}",
            output::render_table(&["NAME", "SIZE", "STATUS"], &rows)
        );
        return Ok(());
    }
    let out = args.output_dir.unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&out)?;
    let mut got = 0usize;
    for a in &arts {
        if let Some(filter) = &args.name {
            if &a.name != filter {
                continue;
            }
        }
        if a.status == "expired" {
            println!("{}", output::dim(&format!("skipping {} (expired)", a.name)));
            continue;
        }
        let dest = artifact_dest(Path::new(&out), &a.name)?;
        let bytes = api::workflow::download_artifact(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            args.run_id,
            &a.name,
        )
        .await?;
        std::fs::write(&dest, &bytes)?;
        println!("✓ Downloaded {} → {}", a.name, dest.display());
        got += 1;
    }
    if got == 0 {
        println!("(no artifacts matched)");
    }
    Ok(())
}

/// Destination path for an artifact zip, rejecting names that try to escape
/// the output directory.
fn artifact_dest(dir: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(anyhow!("refusing unsafe artifact name {name:?}"));
    }
    Ok(dir.join(format!("{name}.zip")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_dest_accepts_plain_names() {
        let dir = Path::new("/tmp/out");
        assert_eq!(
            artifact_dest(dir, "build-logs").unwrap(),
            dir.join("build-logs.zip")
        );
    }

    #[test]
    fn artifact_dest_rejects_unsafe_names() {
        let dir = Path::new("/tmp/out");
        for bad in ["", "../escape", "a/b", "a\\b", "..", "x..y"] {
            assert!(
                artifact_dest(dir, bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }
}
