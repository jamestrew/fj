//! Token verification + server-version probing shared by login and refresh.

use anyhow::Result;

use crate::client::{Client, ResolvedHost};
use crate::config::hosts::Host;
use crate::output;

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn probe_version(client: &Client) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct V {
        version: String,
    }
    let v: V = client
        .json(reqwest::Method::GET, "/api/v1/version", &[], None::<&()>)
        .await?;
    Ok(v.version)
}

/// `version_string` looks like "7.0.16+gitea-1.21.11" or "1.21.11". We warn
/// when the leading numeric component looks pre-7.x. Anything we can't parse
/// gets a silent pass since exotic Forgejo-likes still mostly work.
pub(crate) fn warn_on_old_forgejo(version_string: &str) {
    if let Some(msg) = version_warning(version_string) {
        eprintln!("{} {msg}", output::bold("warning:"));
    }
}

/// Pure helper. Returns the human-readable warning (without the "warning:"
/// prefix) when the server's version is older than the fj baseline. Returns
/// `None` when the version is unparseable or fresh enough.
fn version_warning(version_string: &str) -> Option<String> {
    if version_string.is_empty() {
        return None;
    }
    let head = version_string
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .next()
        .unwrap_or("");
    let major: u32 = head
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if major == 0 || major >= 7 {
        return None;
    }
    Some(format!(
        "server reports version `{version_string}`, which is older than the Forgejo 7.x baseline fj is tested against. Some commands (pr ready, repo mirror-sync, search code, milestone) may 404. See docs/compatibility.md."
    ))
}

pub(crate) fn build_probe_client(host: &str, cfg: &Host, token: &str) -> Result<Client> {
    let resolved = ResolvedHost {
        name: host.to_string(),
        host: cfg.clone(),
        token: Some(token.to_string()),
    };
    Client::new(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_warning_fires_on_old() {
        assert!(version_warning("1.21.11").is_some());
        assert!(version_warning("5.0.0").is_some());
        assert!(version_warning("6.0.0+gitea-1.20.0").is_some());
    }

    #[test]
    fn version_warning_silent_on_modern() {
        assert!(version_warning("7.0.16+gitea-1.21.11").is_none());
        assert!(version_warning("8.0.0").is_none());
        assert!(version_warning("10.5.2-beta").is_none());
    }

    #[test]
    fn version_warning_silent_on_unparseable() {
        assert!(version_warning("").is_none());
        assert!(version_warning("totally-unknown").is_none());
        // Hardcoded 0.x major never triggers the warning either (likely a
        // dev / preview build we don't want to be noisy about).
        assert!(version_warning("0.99.0").is_none());
    }
}
