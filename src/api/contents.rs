//! Repository file and directory reads at a ref
//! (`GET /repos/{owner}/{repo}/contents[/{filepath}]?ref=`).
//!
//! Read-only on purpose: fj has no server-side file edit, so a tag (which is
//! immutable) and a branch are read through exactly the same call.

use anyhow::{Context, Result};
use base64::Engine;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentEntry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    /// `file`, `dir`, `symlink`, or `submodule`.
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub sha: String,
    /// Populated for a single file read; `null` in directory listings and for
    /// blobs over the server's API size cap.
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub encoding: Option<String>,
    /// Symlink target.
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub submodule_git_url: Option<String>,
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub last_commit_sha: Option<String>,
}

/// The contents endpoint answers a directory with an array and anything else
/// with a single object.
#[derive(Debug, Clone)]
pub enum Contents {
    Dir(Vec<ContentEntry>),
    Entry(Box<ContentEntry>),
}

/// API path for a repo-relative file path. Each segment is percent-encoded on
/// its own so a `/` still separates directories while spaces, `#`, `?` and the
/// like cannot leak into the URL structure. An empty path is the repo root.
pub fn contents_path(owner: &str, name: &str, filepath: &str) -> String {
    let base = format!("{}/contents", crate::api::repo_path(owner, name));
    let encoded: Vec<String> = filepath
        .split('/')
        .filter(|s| !s.is_empty())
        .map(crate::api::path_segment)
        .collect();
    if encoded.is_empty() {
        base
    } else {
        format!("{base}/{}", encoded.join("/"))
    }
}

/// Read a file or list a directory. `git_ref` is a branch name, tag name, or
/// commit SHA; `None` reads the default branch.
pub async fn get(
    client: &Client,
    owner: &str,
    name: &str,
    filepath: &str,
    git_ref: Option<&str>,
) -> Result<Contents> {
    let path = contents_path(owner, name, filepath);
    let query: Vec<(String, String)> = git_ref
        .map(|r| vec![("ref".to_string(), r.to_string())])
        .unwrap_or_default();
    let value: serde_json::Value = client
        .json(Method::GET, &path, &query, None::<&()>)
        .await
        .map_err(|e| {
            let shown = if filepath.is_empty() { "/" } else { filepath };
            let at = git_ref.map(|r| format!(" at '{r}'")).unwrap_or_default();
            crate::client::with_not_found_context(e, format!("path '{shown}' not found{at}"))
        })?;
    decode(value)
}

fn decode(value: serde_json::Value) -> Result<Contents> {
    if value.is_array() {
        Ok(Contents::Dir(
            serde_json::from_value(value).context("decoding directory listing")?,
        ))
    } else {
        Ok(Contents::Entry(Box::new(
            serde_json::from_value(value).context("decoding contents response")?,
        )))
    }
}

/// Decode a file entry's bytes. `None` when the server sent no content (a
/// blob over its API size cap).
pub fn file_bytes(entry: &ContentEntry) -> Result<Option<Vec<u8>>> {
    let Some(content) = entry.content.as_deref() else {
        return Ok(None);
    };
    if entry.encoding.as_deref() != Some("base64") {
        return Ok(Some(content.as_bytes().to_vec()));
    }
    let compact: String = content.lines().map(str::trim).collect();
    base64::engine::general_purpose::STANDARD
        .decode(compact)
        .map(Some)
        .with_context(|| format!("decoding base64 contents for {}", entry.path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn contents_path_encodes_each_segment_and_keeps_separators() {
        assert_eq!(
            contents_path("o", "r", "docs/My File#1.md"),
            "/api/v1/repos/o/r/contents/docs/My%20File%231.md"
        );
        assert_eq!(contents_path("o", "r", ""), "/api/v1/repos/o/r/contents");
        assert_eq!(
            contents_path("o", "r", "/src/"),
            "/api/v1/repos/o/r/contents/src"
        );
    }

    #[test]
    fn decode_tells_a_directory_from_a_file() {
        let dir = decode(json!([{ "name": "src", "path": "src", "type": "dir" }])).unwrap();
        assert!(matches!(dir, Contents::Dir(ref v) if v.len() == 1 && v[0].kind == "dir"));
        let file = decode(json!({ "name": "a", "path": "a", "type": "file", "size": 2 })).unwrap();
        assert!(matches!(file, Contents::Entry(ref e) if e.kind == "file" && e.size == 2));
    }

    #[test]
    fn file_bytes_decodes_wrapped_base64_and_reports_missing_content() {
        let entry = ContentEntry {
            content: Some("aGVs\nbG8=\n".into()),
            encoding: Some("base64".into()),
            ..Default::default()
        };
        assert_eq!(file_bytes(&entry).unwrap().unwrap(), b"hello");
        assert!(file_bytes(&ContentEntry::default()).unwrap().is_none());
        let bad = ContentEntry {
            content: Some("!!".into()),
            encoding: Some("base64".into()),
            ..Default::default()
        };
        assert!(file_bytes(&bad).is_err());
    }
}
