//! `fj config` — local preferences (editor, pager, browser, default merge style).
//!
//! Stored in `$XDG_CONFIG_HOME/fj/config.toml`. These keys are advisory: the
//! relevant subcommands look them up when their corresponding env var is unset.

use std::collections::BTreeMap;
use std::fs;

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

const KNOWN_KEYS: &[&str] = &[
    "editor",
    "pager",
    "browser",
    "git_protocol",
    "default_merge",
    "no_color",
];

#[derive(Debug, Args)]
pub struct ConfigCmd {
    #[command(subcommand)]
    pub command: ConfigSub,
}

#[derive(Debug, Subcommand)]
pub enum ConfigSub {
    /// Print a key's current value.
    Get(GetArgs),
    /// Set a key.
    Set(SetArgs),
    /// Unset a key.
    Unset(GetArgs),
    /// Remove a key.
    Remove(GetArgs),
    /// Show all configured keys.
    List,
    /// Print the path of the config file.
    Path,
}

#[derive(Debug, Args)]
pub struct GetArgs {
    /// Configuration key
    pub key: String,
}

#[derive(Debug, Args)]
pub struct SetArgs {
    /// Configuration key
    pub key: String,
    /// Value to set
    pub value: String,
}

pub fn config_path() -> Result<std::path::PathBuf> {
    Ok(config::config_dir()?.join("config.toml"))
}

pub fn load() -> Result<ConfigFile> {
    let p = config_path()?;
    if !p.exists() {
        return Ok(ConfigFile::default());
    }
    let text = fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", p.display()))
}

pub fn save(c: &ConfigFile) -> Result<()> {
    let p = config_path()?;
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&p, toml::to_string_pretty(c)?)?;
    Ok(())
}

/// Read a config value, optionally falling back to an env var.
#[allow(dead_code)]
pub fn lookup(key: &str, env_fallback: Option<&str>) -> Option<String> {
    if let Some(env) = env_fallback {
        if let Ok(v) = std::env::var(env) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    load().ok().and_then(|c| c.values.get(key).cloned())
}

pub fn no_color_enabled() -> bool {
    lookup("no_color", None)
        .map(|value| matches_truthy(&value))
        .unwrap_or(false)
}

fn matches_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn validate_key(key: &str) -> Result<()> {
    if KNOWN_KEYS.contains(&key) {
        Ok(())
    } else {
        Err(anyhow!(
            "unknown config key '{key}' (known: {})",
            KNOWN_KEYS.join(", ")
        ))
    }
}

fn set_value(file: &mut ConfigFile, key: &str, value: &str) -> Result<()> {
    validate_key(key)?;
    file.values.insert(key.to_string(), value.to_string());
    Ok(())
}

fn unset_value(file: &mut ConfigFile, key: &str) -> Result<()> {
    file.values.remove(key);
    Ok(())
}

pub async fn run(cmd: ConfigCmd) -> Result<()> {
    match cmd.command {
        ConfigSub::Get(args) => {
            let file = load()?;
            match file.values.get(&args.key) {
                Some(v) => println!("{v}"),
                None => return Err(anyhow!("key '{}' is unset", args.key)),
            }
            Ok(())
        }
        ConfigSub::Set(args) => {
            validate_key(&args.key)?;
            let mut file = load()?;
            set_value(&mut file, &args.key, &args.value)?;
            save(&file)?;
            println!("✓ {} = {}", args.key, args.value);
            Ok(())
        }
        ConfigSub::Unset(args) | ConfigSub::Remove(args) => {
            validate_key(&args.key)?;
            let mut file = load()?;
            unset_value(&mut file, &args.key)?;
            save(&file)?;
            println!("✓ unset {}", args.key);
            Ok(())
        }
        ConfigSub::List => {
            let file = load()?;
            if file.values.is_empty() {
                println!("(empty)");
                return Ok(());
            }
            for (k, v) in &file.values {
                println!("{k} = {v}");
            }
            Ok(())
        }
        ConfigSub::Path => {
            println!("{}", config_path()?.display());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_set_rejects_unknown_keys() {
        let mut file = ConfigFile::default();
        let err = set_value(&mut file, "typo_key", "value").expect_err("unknown key rejected");
        assert!(err.to_string().contains("unknown config key"));
        assert!(file.values.is_empty());
    }

    #[test]
    fn config_unset_removes_key() {
        let mut file = ConfigFile::default();
        set_value(&mut file, "editor", "vim").unwrap();
        unset_value(&mut file, "editor").unwrap();
        assert!(!file.values.contains_key("editor"));
    }
}
