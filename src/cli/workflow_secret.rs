//! `fj secret` — Forgejo Actions secrets (list/set/delete).
//!
//! Hosts the shared `SecretListArgs` / `SecretSetArgs` / `SecretDeleteArgs`
//! and `read_value`, reused by the variable group in `workflow_variable`.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::output;

#[derive(Debug, Args)]
pub struct SecretCmd {
    #[command(subcommand)]
    pub command: SecretSub,
}

#[derive(Debug, Subcommand)]
pub enum SecretSub {
    /// List secret names (values are write-only).
    List(SecretListArgs),
    /// Set or replace a secret.
    Set(SecretSetArgs),
    /// Delete a secret.
    Delete(SecretDeleteArgs),
}

#[derive(Debug, Args)]
pub struct SecretListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
}

#[derive(Debug, Args)]
pub struct SecretSetArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub name: String,
    /// Value (raw string). For multi-line secrets, prefer `--from-file -` and pipe stdin.
    #[arg(short = 'v', long)]
    pub value: Option<String>,
    /// Read value from this file. Use `-` for stdin.
    #[arg(short = 'f', long = "from-file")]
    pub from_file: Option<String>,
}

#[derive(Debug, Args)]
pub struct SecretDeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub name: String,
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run_secret(cmd: SecretCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        SecretSub::List(args) => secret_list(args, host).await,
        SecretSub::Set(args) => secret_set(args, host).await,
        SecretSub::Delete(args) => secret_delete(args, host).await,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn secret_list(args: SecretListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let s = api::workflow::list_secrets(&ctx.client, &ctx.owner, &ctx.name).await?;
    if s.is_empty() {
        println!("(no secrets)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = s
        .iter()
        .map(|x| {
            vec![
                x.name.clone(),
                output::dim(&output::relative_time(x.created_at)),
            ]
        })
        .collect();
    print!("{}", output::render_table(&["NAME", "CREATED"], &rows));
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn secret_set(args: SecretSetArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let value = read_value(args.value.as_deref(), args.from_file.as_deref())?;
    api::workflow::set_secret(&ctx.client, &ctx.owner, &ctx.name, &args.name, &value).await?;
    println!("✓ Set secret {}", args.name);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn secret_delete(args: SecretDeleteArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if !args.yes {
        let ans = crate::cli::editor::prompt_line(&format!(
            "Delete secret '{}'? Type name to confirm",
            args.name
        ))?;
        if ans != args.name {
            return Err(anyhow::anyhow!("aborted"));
        }
    }
    api::workflow::delete_secret(&ctx.client, &ctx.owner, &ctx.name, &args.name).await?;
    println!("✓ Deleted secret {}", args.name);
    Ok(())
}

pub(crate) fn read_value(value: Option<&str>, from_file: Option<&str>) -> Result<String> {
    if let Some(v) = value {
        return Ok(v.to_string());
    }
    if let Some(path) = from_file {
        if path == "-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            return Ok(buf.trim_end().to_string());
        }
        return Ok(std::fs::read_to_string(path)?.trim_end().to_string());
    }
    Err(anyhow::anyhow!(
        "pass --value or --from-file (use `-` for stdin)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_value_passes_inline_value_through_verbatim() {
        // Inline values are intentionally not trimmed: a secret may have
        // meaningful leading/trailing whitespace.
        assert_eq!(read_value(Some("  s3cret  "), None).unwrap(), "  s3cret  ");
    }

    #[test]
    fn read_value_reads_file_and_trims_trailing_newline() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "tokenvalue").unwrap();
        let got = read_value(None, Some(f.path().to_str().unwrap())).unwrap();
        assert_eq!(got, "tokenvalue");
    }

    #[test]
    fn read_value_requires_a_source() {
        assert!(read_value(None, None).is_err());
    }

    #[test]
    fn read_value_prefers_inline_over_file() {
        let got = read_value(Some("inline"), Some("/nonexistent/path")).unwrap();
        assert_eq!(got, "inline");
    }
}
