//! Release asset downloads for a Fjord Account host.
//!
//! A release asset's `browser_download_url` is on the forge's web origin, which
//! never accepts the Fjord bearer (and must never be sent it). The gateway
//! already forwards any path to the instance with a per-user forge token, so
//! the asset is fetched from there instead. The gateway has no path allowlist,
//! so the path is checked here first: it must be exactly the download route of
//! the release being downloaded, on the origin that release names.

use anyhow::{anyhow, Result};
use percent_encoding::percent_decode_str;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT};

use super::*;

impl Client {
    /// The gateway URL for `asset_url`, or `None` when this client does not go
    /// through the gateway (the caller then fetches the URL directly).
    pub fn gateway_release_asset(
        &self,
        (owner, repo): (&str, &str),
        release_html_url: &str,
        asset_url: &str,
        tag: &str,
        asset_name: &str,
    ) -> Result<Option<Url>> {
        if self.auth_scheme != AuthScheme::FjordBearer {
            return Ok(None);
        }
        let refuse = || {
            anyhow!(
                "refusing to download '{asset_name}': its URL is not this release's download route"
            )
        };
        let (asset, release) = (Url::parse(asset_url)?, Url::parse(release_html_url)?);
        if !same_origin(&asset, &release) || asset.query().is_some() || asset.fragment().is_some() {
            return Err(refuse());
        }
        // The release's own page and the asset must both sit directly under
        // /<owner>/<repo>. An instance mounted under a base path is refused: the
        // gateway appends the path to the instance URL itself, and fj does not
        // know that base, so it cannot say how much of the path to strip.
        let decode = |u: &Url| {
            percent_decode_str(u.path())
                .decode_utf8()
                .map(|p| p.into_owned())
        };
        let (decoded, page) = (
            decode(&asset).map_err(|_| refuse())?,
            decode(&release).map_err(|_| refuse())?,
        );
        let prefix = page
            .strip_suffix(&format!("/releases/tag/{tag}"))
            .ok_or_else(refuse)?;
        let want = format!("{prefix}/releases/download/{tag}/{asset_name}");
        if decoded != want
            || !prefix.eq_ignore_ascii_case(&format!("/{owner}/{repo}"))
            || decoded.split('/').any(|s| s == "." || s == "..")
        {
            return Err(refuse());
        }
        // Raw path, as the server wrote it: it has just been matched.
        Ok(Some(
            self.base
                .join("../../")?
                .join(asset.path().trim_start_matches('/'))?,
        ))
    }

    /// GET a validated gateway asset URL with the Fjord bearer, refresh and
    /// account checks included.
    pub async fn get_release_asset(&self, gateway_url: &Url) -> Result<Response> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
        self.request_with_headers(Method::GET, gateway_url.as_str(), &[], None, &headers)
            .await
    }
}

#[cfg(test)]
#[path = "release_asset_tests.rs"]
mod tests;
