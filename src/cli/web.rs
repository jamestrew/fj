//! Open URLs in the user's default browser, like `gh ... --web`.

use std::process::Command;

use anyhow::{anyhow, Context, Result};

pub fn open(url: &str) -> Result<()> {
    let (program, args) = picker();
    let status = Command::new(program)
        .args(args)
        .arg(url)
        .status()
        .with_context(|| format!("launching `{program}` to open URL"))?;
    if !status.success() {
        return Err(anyhow!("browser open command exited with status {status}"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn picker() -> (&'static str, &'static [&'static str]) {
    ("open", &[])
}

#[cfg(target_os = "linux")]
fn picker() -> (&'static str, &'static [&'static str]) {
    ("xdg-open", &[])
}

#[cfg(target_os = "windows")]
fn picker() -> (&'static str, &'static [&'static str]) {
    ("cmd", &["/C", "start"])
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn picker() -> (&'static str, &'static [&'static str]) {
    ("xdg-open", &[])
}
