//! `fj alias` — user-defined command shortcuts stored in $XDG_CONFIG_HOME/fj/aliases.toml.
//!
//! `fj alias set co "pr checkout"` then `fj co 42` (note: top-level shadowing
//! of unknown subcommands is implemented in main.rs by checking the alias map
//! before clap parsing).

use std::collections::BTreeMap;
use std::fs;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use crate::config;
use crate::output;

#[derive(Debug, Args)]
pub struct AliasCmd {
    #[command(subcommand)]
    pub command: AliasSub,
}

#[derive(Debug, Subcommand)]
pub enum AliasSub {
    /// List configured aliases.
    List,
    /// Set an alias (e.g. `fj alias set co "pr checkout"`).
    Set(SetArgs),
    /// Delete an alias.
    Delete(DeleteArgs),
}

#[derive(Debug, Args)]
pub struct SetArgs {
    pub name: String,
    /// Expansion: the rest of the line after the alias name.
    pub expansion: String,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    pub name: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AliasFile {
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
}

pub fn path() -> Result<std::path::PathBuf> {
    Ok(config::config_dir()?.join("aliases.toml"))
}

pub fn load() -> Result<AliasFile> {
    let p = path()?;
    if !p.exists() {
        return Ok(AliasFile::default());
    }
    let text = fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", p.display()))
}

pub fn save(a: &AliasFile) -> Result<()> {
    let p = path()?;
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&p, toml::to_string_pretty(a)?)?;
    Ok(())
}

pub async fn run(cmd: AliasCmd) -> Result<()> {
    let mut file = load()?;
    match cmd.command {
        AliasSub::List => list(&file),
        AliasSub::Set(args) => {
            file.aliases
                .insert(args.name.clone(), args.expansion.clone());
            save(&file)?;
            println!("✓ {} → {}", args.name, args.expansion);
            Ok(())
        }
        AliasSub::Delete(args) => {
            if file.aliases.remove(&args.name).is_none() {
                return Err(anyhow::anyhow!("no alias '{}'", args.name));
            }
            save(&file)?;
            println!("✓ deleted {}", args.name);
            Ok(())
        }
    }
}

fn list(file: &AliasFile) -> Result<()> {
    if file.aliases.is_empty() {
        println!("(no aliases)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = file
        .aliases
        .iter()
        .map(|(k, v)| vec![k.clone(), v.clone()])
        .collect();
    print!("{}", output::render_table(&["ALIAS", "EXPANSION"], &rows));
    Ok(())
}

/// Expand `argv` if `argv[1]` is a configured alias. Returns a new argv with
/// the alias replaced by its tokenized expansion. Falls back to `argv` if no
/// alias matches.
pub fn expand_argv(argv: Vec<String>) -> Vec<String> {
    if argv.len() < 2 {
        return argv;
    }
    let Ok(file) = load() else {
        return argv;
    };
    let Some(expansion) = file.aliases.get(&argv[1]) else {
        return argv;
    };
    let mut out = Vec::with_capacity(argv.len() + 4);
    out.push(argv[0].clone());
    for tok in expansion.split_whitespace() {
        out.push(tok.to_string());
    }
    out.extend(argv.into_iter().skip(2));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_no_alias_passthrough() {
        let argv = vec!["fj".into(), "pr".into(), "list".into()];
        assert_eq!(expand_argv(argv.clone()), argv);
    }

    #[test]
    fn expand_empty_argv() {
        assert_eq!(expand_argv(vec!["fj".into()]), vec!["fj".to_string()]);
    }
}
