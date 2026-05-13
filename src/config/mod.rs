pub mod hosts;

use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;

/// Returns the directory where fj stores its configuration files.
pub fn config_dir() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("com", "rasterstate", "fj")
        .context("could not determine a platform config directory for fj")?;
    Ok(dirs.config_dir().to_path_buf())
}

pub fn hosts_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("hosts.toml"))
}
