//! `fj extension` — discover and run user-installed plugins.
//!
//! Convention matches gh: any executable on $PATH named `fj-<name>` is treated
//! as a plugin. `fj extension list` enumerates them. The shell does the rest
//! of the work since you can already invoke `fj-foo` directly; this command
//! just makes them discoverable from inside `fj`.

use std::collections::BTreeSet;
use std::env;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::output;

#[derive(Debug, Args)]
pub struct ExtensionCmd {
    #[command(subcommand)]
    pub command: ExtensionSub,
}

#[derive(Debug, Subcommand)]
pub enum ExtensionSub {
    /// List discovered `fj-*` extensions on PATH.
    List,
    /// Invoke a discovered extension. Equivalent to running `fj-<name> ...`.
    Run(RunArgs),
}

#[derive(Debug, Args)]
pub struct RunArgs {
    pub name: String,
    /// Arguments forwarded verbatim to the plugin binary.
    pub args: Vec<String>,
}

pub async fn run(cmd: ExtensionCmd) -> Result<()> {
    match cmd.command {
        ExtensionSub::List => list().await,
        ExtensionSub::Run(args) => invoke(args).await,
    }
}

async fn list() -> Result<()> {
    let mut found: BTreeSet<(String, PathBuf)> = BTreeSet::new();
    let path = env::var_os("PATH").unwrap_or_default();
    for dir in env::split_paths(&path) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(plugin) = name.strip_prefix("fj-") {
                if !plugin.is_empty() && !plugin.starts_with('.') {
                    found.insert((plugin.to_string(), entry.path()));
                }
            }
        }
    }
    if found.is_empty() {
        println!("(no fj-* extensions found on PATH)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = found
        .iter()
        .map(|(n, p)| vec![n.clone(), output::dim(&p.display().to_string())])
        .collect();
    print!("{}", output::render_table(&["NAME", "PATH"], &rows));
    Ok(())
}

async fn invoke(args: RunArgs) -> Result<()> {
    let prog = format!("fj-{}", args.name);
    let status = std::process::Command::new(&prog)
        .args(&args.args)
        .status()?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}
