use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub id: u64,
    pub tag_name: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub body: String,
    pub target_commitish: String,
    pub draft: bool,
    pub prerelease: bool,
    pub html_url: String,
    pub created_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: u64,
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
    pub created_at: DateTime<Utc>,
}

pub async fn list(client: &Client, owner: &str, name: &str, limit: u32) -> Result<Vec<Release>> {
    let path = format!("/api/v1/repos/{owner}/{name}/releases");
    let query = vec![
        ("limit".into(), limit.clamp(1, 50).to_string()),
        ("page".into(), "1".into()),
    ];
    client.json(Method::GET, &path, &query, None::<&()>).await
}

pub async fn get_by_tag(client: &Client, owner: &str, name: &str, tag: &str) -> Result<Release> {
    let path = format!("/api/v1/repos/{owner}/{name}/releases/tags/{tag}");
    client.json(Method::GET, &path, &[], None::<&()>).await
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateRelease<'a> {
    pub tag_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_commitish: Option<&'a str>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
}

pub async fn create(
    client: &Client,
    owner: &str,
    name: &str,
    body: &CreateRelease<'_>,
) -> Result<Release> {
    let path = format!("/api/v1/repos/{owner}/{name}/releases");
    client.json(Method::POST, &path, &[], Some(body)).await
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct EditRelease<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prerelease: Option<bool>,
}

pub async fn edit(
    client: &Client,
    owner: &str,
    name: &str,
    id: u64,
    body: &EditRelease<'_>,
) -> Result<Release> {
    let path = format!("/api/v1/repos/{owner}/{name}/releases/{id}");
    client.json(Method::PATCH, &path, &[], Some(body)).await
}

pub async fn delete(client: &Client, owner: &str, name: &str, id: u64) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/releases/{id}");
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

/// Upload an asset (binary) to a release.
pub async fn upload_asset(
    client: &Client,
    owner: &str,
    name: &str,
    release_id: u64,
    asset_name: &str,
    bytes: Vec<u8>,
) -> Result<Asset> {
    let path = format!(
        "/api/v1/repos/{owner}/{name}/releases/{release_id}/assets?name={}",
        urlencode(asset_name)
    );
    // Build a multipart form with the file payload.
    let url = client.url(&path)?;
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(asset_name.to_string())
        .mime_str("application/octet-stream")?;
    let form = reqwest::multipart::Form::new().part("attachment", part);
    let mut headers = reqwest::header::HeaderMap::new();
    // Pull the Authorization header straight from the client so it picks
    // up the right scheme (PAT vs Fjord bearer) without this call site
    // needing to know which one is active.
    headers.insert(reqwest::header::AUTHORIZATION, client.auth_header_value()?);
    let res = client
        .http()
        .post(url)
        .headers(headers)
        .multipart(form)
        .send()
        .await?;
    let res = res.error_for_status()?;
    Ok(res.json().await?)
}

/// Delete one asset from a release. Forgejo keys the attachment under its
/// release: `/releases/{release_id}/assets/{asset_id}` (NOT a bare
/// `/releases/assets/{id}`, which 404s).
pub async fn delete_asset(
    client: &Client,
    owner: &str,
    name: &str,
    release_id: u64,
    asset_id: u64,
) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/releases/{release_id}/assets/{asset_id}");
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

fn urlencode(s: &str) -> String {
    // The asset name appears in a query parameter; URL-encode special chars
    // conservatively. `url` crate provides a Serializer for full forms.
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            for b in c.to_string().bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::urlencode;

    #[test]
    fn urlencode_passes_unreserved_chars_through() {
        assert_eq!(
            urlencode("fj-linux_v1.2.3~rc.tar.gz"),
            "fj-linux_v1.2.3~rc.tar.gz"
        );
    }

    #[test]
    fn urlencode_percent_encodes_reserved_and_spaces() {
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("a/b?c=d"), "a%2Fb%3Fc%3Dd");
    }

    #[test]
    fn urlencode_handles_multibyte_utf8_per_byte() {
        // "é" is U+00E9 -> UTF-8 0xC3 0xA9.
        assert_eq!(urlencode("é"), "%C3%A9");
    }
}
