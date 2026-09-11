pub mod hosts;

use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;

/// Returns the directory where fj stores its configuration files.
///
/// `FJ_CONFIG_DIR` overrides it outright. That is what keeps the host
/// resolution tests hermetic on macOS: `ProjectDirs` resolves to
/// `~/Library/Application Support` there and ignores `XDG_CONFIG_HOME`, so a
/// test that thought it had redirected the config directory was reading the
/// developer's real `hosts.toml` instead, and blocking on the login keychain
/// when that named a Fjord-Account host. It is equally useful for a scripted
/// run that wants its own config.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("FJ_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let dirs = ProjectDirs::from("com", "rasterstate", "fj")
        .context("could not determine a platform config directory for fj")?;
    Ok(dirs.config_dir().to_path_buf())
}

pub fn hosts_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("hosts.toml"))
}
