use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    Issue,
    Pull,
}

impl TemplateKind {
    pub fn label(self) -> &'static str {
        match self {
            TemplateKind::Issue => "issue",
            TemplateKind::Pull => "pull request",
        }
    }

    fn fallback_paths(self) -> &'static [&'static str] {
        match self {
            TemplateKind::Issue => &[
                ".forgejo/issue_template",
                ".forgejo/ISSUE_TEMPLATE",
                ".gitea/issue_template",
                ".gitea/ISSUE_TEMPLATE",
                ".github/ISSUE_TEMPLATE",
                ".forgejo/ISSUE_TEMPLATE.md",
                ".gitea/ISSUE_TEMPLATE.md",
                ".github/ISSUE_TEMPLATE.md",
                "ISSUE_TEMPLATE.md",
            ],
            TemplateKind::Pull => &[
                ".forgejo/pull_request_template.md",
                ".forgejo/PULL_REQUEST_TEMPLATE.md",
                ".gitea/PULL_REQUEST_TEMPLATE.md",
                ".github/PULL_REQUEST_TEMPLATE.md",
                ".github/PULL_REQUEST_TEMPLATE",
                "PULL_REQUEST_TEMPLATE.md",
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoTemplate {
    pub name: String,
    pub file_name: String,
    pub content: String,
}

impl RepoTemplate {
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.file_name
        } else {
            &self.name
        }
    }
}

#[derive(Debug, Deserialize)]
struct IssueTemplateResponse {
    #[serde(default)]
    name: String,
    #[serde(default)]
    file_name: String,
    #[serde(default)]
    content: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ContentsResponse {
    File(ContentItem),
    Dir(Vec<ContentItem>),
}

#[derive(Debug, Deserialize)]
struct ContentItem {
    #[serde(default)]
    name: String,
    #[serde(default)]
    path: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    encoding: Option<String>,
}

pub async fn list(
    client: &Client,
    owner: &str,
    repo: &str,
    kind: TemplateKind,
) -> Result<Vec<RepoTemplate>> {
    if kind == TemplateKind::Issue {
        let from_api = list_issue_templates_api(client, owner, repo).await?;
        if !from_api.is_empty() {
            return Ok(from_api);
        }
    }

    let default_branch = super::repo::get(client, owner, repo).await?.default_branch;
    list_from_contents(client, owner, repo, kind, &default_branch).await
}

pub fn select<'a>(
    templates: &'a [RepoTemplate],
    kind: TemplateKind,
    requested: &str,
) -> Result<&'a RepoTemplate> {
    let requested = requested.trim();
    if requested.is_empty() {
        bail!("--template requires a name when stdin is not interactive");
    }
    if let Ok(index) = requested.parse::<usize>() {
        if (1..=templates.len()).contains(&index) {
            return Ok(&templates[index - 1]);
        }
    }
    templates
        .iter()
        .find(|template| matches_template(template, requested))
        .ok_or_else(|| {
            let names = templates
                .iter()
                .map(|t| t.display_name().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            if names.is_empty() {
                anyhow!("no {} templates found", kind.label())
            } else {
                anyhow!(
                    "no {} template named '{}'. Available: {}",
                    kind.label(),
                    requested,
                    names
                )
            }
        })
}

fn matches_template(template: &RepoTemplate, requested: &str) -> bool {
    let requested = requested.trim();
    let requested_lower = requested.to_ascii_lowercase();
    template.name.eq_ignore_ascii_case(requested)
        || template.file_name.eq_ignore_ascii_case(requested)
        || basename(&template.file_name).eq_ignore_ascii_case(requested)
        || stem(&template.file_name).eq_ignore_ascii_case(&requested_lower)
}

async fn list_issue_templates_api(
    client: &Client,
    owner: &str,
    repo: &str,
) -> Result<Vec<RepoTemplate>> {
    let path = format!("{}/issue_templates", super::repo_path(owner, repo));
    let res = client.request(Method::GET, &path, &[], None).await?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    let res = crate::client::ensure_success(res).await?;
    let rows: Vec<IssueTemplateResponse> = res
        .json()
        .await
        .context("decoding issue templates response")?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let content = trim_template_content(row.content);
            (!content.is_empty()).then_some(RepoTemplate {
                name: row.name,
                file_name: row.file_name,
                content,
            })
        })
        .collect())
}

async fn list_from_contents(
    client: &Client,
    owner: &str,
    repo: &str,
    kind: TemplateKind,
    default_branch: &str,
) -> Result<Vec<RepoTemplate>> {
    let mut out = Vec::new();
    for path in kind.fallback_paths() {
        if path.ends_with(".md") {
            if let Some(template) =
                read_template_file(client, owner, repo, path, default_branch).await?
            {
                out.push(template);
            }
            continue;
        }
        out.extend(list_template_dir(client, owner, repo, path, default_branch).await?);
    }
    Ok(out)
}

async fn list_template_dir(
    client: &Client,
    owner: &str,
    repo: &str,
    dir: &str,
    default_branch: &str,
) -> Result<Vec<RepoTemplate>> {
    let Some(ContentsResponse::Dir(items)) =
        read_contents(client, owner, repo, dir, default_branch).await?
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for item in items {
        if item.kind == "file" && is_template_file(&item.name) {
            if let Some(template) =
                read_template_file(client, owner, repo, &item.path, default_branch).await?
            {
                out.push(template);
            }
        }
    }
    Ok(out)
}

async fn read_template_file(
    client: &Client,
    owner: &str,
    repo: &str,
    path: &str,
    default_branch: &str,
) -> Result<Option<RepoTemplate>> {
    let Some(ContentsResponse::File(item)) =
        read_contents(client, owner, repo, path, default_branch).await?
    else {
        return Ok(None);
    };
    if item.kind != "file" {
        return Ok(None);
    }
    let content = decode_content(&item)?.trim().to_string();
    if content.is_empty() {
        return Ok(None);
    }
    Ok(Some(RepoTemplate {
        name: stem(&item.path),
        file_name: item.path,
        content,
    }))
}

async fn read_contents(
    client: &Client,
    owner: &str,
    repo: &str,
    path: &str,
    default_branch: &str,
) -> Result<Option<ContentsResponse>> {
    let api_path = format!("{}/contents/{path}", super::repo_path(owner, repo));
    let q = vec![("ref".to_string(), default_branch.to_string())];
    let res = client.request(Method::GET, &api_path, &q, None).await?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let res = crate::client::ensure_success(res).await?;
    Ok(Some(res.json().await.with_context(|| {
        format!("decoding contents response for {path}")
    })?))
}

fn decode_content(item: &ContentItem) -> Result<String> {
    let content = item.content.as_deref().unwrap_or_default();
    if item.encoding.as_deref() != Some("base64") {
        return Ok(content.to_string());
    }
    let compact = content.lines().map(str::trim).collect::<String>();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(compact)
        .with_context(|| format!("decoding base64 contents for {}", item.path))?;
    String::from_utf8(bytes).with_context(|| format!("decoding UTF-8 contents for {}", item.path))
}

fn trim_template_content(content: String) -> String {
    content.trim().to_string()
}

fn is_template_file(name: &str) -> bool {
    name.ends_with(".md") || name.ends_with(".yaml") || name.ends_with(".yml")
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn stem(path: &str) -> String {
    let base = basename(path);
    base.rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(base)
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(name: &str, file_name: &str) -> RepoTemplate {
        RepoTemplate {
            name: name.into(),
            file_name: file_name.into(),
            content: "body".into(),
        }
    }

    #[test]
    fn select_matches_name_path_basename_stem_and_number() {
        let templates = vec![template("Bug report", ".forgejo/issue_template/bug.md")];
        assert_eq!(
            select(&templates, TemplateKind::Issue, "Bug report")
                .unwrap()
                .file_name,
            ".forgejo/issue_template/bug.md"
        );
        assert_eq!(
            select(&templates, TemplateKind::Issue, "bug.md")
                .unwrap()
                .name,
            "Bug report"
        );
        assert_eq!(
            select(&templates, TemplateKind::Issue, "bug").unwrap().name,
            "Bug report"
        );
        assert_eq!(
            select(&templates, TemplateKind::Issue, "1").unwrap().name,
            "Bug report"
        );
    }

    #[test]
    fn select_reports_available_names() {
        let templates = vec![template("Feature", "feature.md")];
        let err = select(&templates, TemplateKind::Issue, "bug").unwrap_err();
        assert!(err.to_string().contains("Available: Feature"));
    }
}
