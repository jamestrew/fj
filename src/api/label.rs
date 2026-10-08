use anyhow::{anyhow, Result};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

pub use super::issue::Label;

#[derive(Debug, Clone, Serialize)]
pub struct CreateLabel<'a> {
    pub name: &'a str,
    pub color: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    /// Make the label one of an exclusive group (`scope/name`). Omitted from
    /// the body when `None` so the server default (not exclusive) applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct EditLabel<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive: Option<bool>,
}

fn repo_labels_path(owner: &str, name: &str) -> String {
    format!("{}/labels", crate::api::repo_path(owner, name))
}

fn org_labels_path(org: &str) -> String {
    format!("/api/v1/orgs/{}/labels", crate::api::path_segment(org))
}

/// Every label defined on the repository itself. Organization labels are a
/// separate collection (see [`list_org`]) and are not included.
pub async fn list(client: &Client, owner: &str, name: &str) -> Result<Vec<Label>> {
    Ok(client
        .get_all_page(&repo_labels_path(owner, name), &[], None)
        .await?
        .items)
}

pub async fn get_by_name(
    client: &Client,
    owner: &str,
    name: &str,
    label_name: &str,
) -> Result<Label> {
    list(client, owner, name)
        .await?
        .into_iter()
        .find(|l| l.name == label_name)
        .ok_or_else(|| anyhow!("no label named '{label_name}'"))
}

/// Resolve label names to the labels an issue or PR in this repo can carry:
/// the repo's own labels first, then (only if a name is still unmatched) the
/// owning organization's labels. A repo label shadows an org label of the
/// same name, which is how Forgejo's own picker orders them.
pub async fn resolve_names(
    client: &Client,
    owner: &str,
    name: &str,
    label_names: &[String],
) -> Result<Vec<Label>> {
    let repo_labels = list(client, owner, name).await?;
    let find = |pool: &[Label], wanted: &str| pool.iter().find(|l| l.name == wanted).cloned();
    let mut org_labels: Option<Vec<Label>> = None;
    let mut out = Vec::with_capacity(label_names.len());
    for label_name in label_names {
        if let Some(l) = find(&repo_labels, label_name) {
            out.push(l);
            continue;
        }
        if org_labels.is_none() {
            org_labels = Some(list_org_if_org(client, owner).await?);
        }
        let l = find(org_labels.as_deref().unwrap_or_default(), label_name)
            .ok_or_else(|| anyhow!("no label named '{label_name}'"))?;
        out.push(l);
    }
    Ok(out)
}

pub async fn resolve_names_to_ids(
    client: &Client,
    owner: &str,
    name: &str,
    label_names: &[String],
) -> Result<Vec<u64>> {
    Ok(resolve_names(client, owner, name, label_names)
        .await?
        .into_iter()
        .map(|l| l.id)
        .collect())
}

pub async fn create(
    client: &Client,
    owner: &str,
    name: &str,
    body: &CreateLabel<'_>,
) -> Result<Label> {
    client
        .json(
            Method::POST,
            &repo_labels_path(owner, name),
            &[],
            Some(body),
        )
        .await
}

pub async fn edit(
    client: &Client,
    owner: &str,
    name: &str,
    id: u64,
    body: &EditLabel<'_>,
) -> Result<Label> {
    let path = format!("{}/{id}", repo_labels_path(owner, name));
    client.json(Method::PATCH, &path, &[], Some(body)).await
}

pub async fn delete(client: &Client, owner: &str, name: &str, id: u64) -> Result<()> {
    let path = format!("{}/{id}", repo_labels_path(owner, name));
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

// ---- organization labels ----

/// Every label defined on the organization. These are shared by all of the
/// organization's repositories and are managed apart from repo labels.
pub async fn list_org(client: &Client, org: &str) -> Result<Vec<Label>> {
    Ok(client
        .get_all_page(&org_labels_path(org), &[], None)
        .await
        .map_err(|e| org_missing(e, org))?
        .items)
}

/// Org labels for a repo owner that may be a user. Forgejo answers 404 for
/// `/orgs/{name}/labels` when the owner is not an organization; that means
/// "no org labels to offer", not a failure.
async fn list_org_if_org(client: &Client, owner: &str) -> Result<Vec<Label>> {
    match client
        .get_all_page(&org_labels_path(owner), &[], None)
        .await
    {
        Ok(page) => Ok(page.items),
        Err(e)
            if e.downcast_ref::<crate::client::ApiError>()
                .is_some_and(|a| a.status == reqwest::StatusCode::NOT_FOUND) =>
        {
            Ok(Vec::new())
        }
        Err(e) => Err(e),
    }
}

pub async fn get_org_by_name(client: &Client, org: &str, label_name: &str) -> Result<Label> {
    list_org(client, org)
        .await?
        .into_iter()
        .find(|l| l.name == label_name)
        .ok_or_else(|| anyhow!("no organization label named '{label_name}' in {org}"))
}

pub async fn create_org(client: &Client, org: &str, body: &CreateLabel<'_>) -> Result<Label> {
    client
        .json(Method::POST, &org_labels_path(org), &[], Some(body))
        .await
        .map_err(|e| org_missing(e, org))
}

pub async fn edit_org(client: &Client, org: &str, id: u64, body: &EditLabel<'_>) -> Result<Label> {
    let path = format!("{}/{id}", org_labels_path(org));
    client
        .json(Method::PATCH, &path, &[], Some(body))
        .await
        .map_err(|e| org_missing(e, org))
}

pub async fn delete_org(client: &Client, org: &str, id: u64) -> Result<()> {
    let path = format!("{}/{id}", org_labels_path(org));
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    crate::client::ensure_success(res)
        .await
        .map_err(|e| org_missing(e, org))?;
    Ok(())
}

fn org_missing(err: anyhow::Error, org: &str) -> anyhow::Error {
    crate::client::with_not_found_context(
        err,
        format!("organization '{org}' or its label was not found (is '{org}' a user account?)"),
    )
}

// ---- labels on an issue or pull request ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddLabelsBody {
    pub labels: Vec<u64>,
}

fn issue_labels_path(owner: &str, name: &str, number: u64) -> String {
    format!(
        "{}/issues/{number}/labels",
        crate::api::repo_path(owner, name)
    )
}

/// The labels currently on an issue or PR.
pub async fn list_on_issue(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
) -> Result<Vec<Label>> {
    Ok(client
        .get_all_page(&issue_labels_path(owner, name, number), &[], None)
        .await?
        .items)
}

/// Add labels. The response is the issue's full label set after the server
/// applied its own rules (an exclusive label displaces its siblings).
pub async fn add_to_issue(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    label_ids: Vec<u64>,
) -> Result<Vec<Label>> {
    client
        .json(
            Method::POST,
            &issue_labels_path(owner, name, number),
            &[],
            Some(&AddLabelsBody { labels: label_ids }),
        )
        .await
}

/// What an add did, in the forge's own terms: the set before (read only when
/// an exclusive label was requested, since nothing else can displace a
/// label), the labels asked for, and the set the server confirmed after.
#[derive(Debug, Clone)]
pub struct Assignment {
    pub before: Option<Vec<Label>>,
    pub requested: Vec<Label>,
    pub after: Vec<Label>,
}

/// Add already-resolved labels to an issue or PR and capture the before and
/// after sets. Exclusivity is never applied client-side: the label ids are
/// sent as asked and the server decides which siblings go.
pub async fn assign(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    requested: Vec<Label>,
) -> Result<Assignment> {
    let before = if requested.iter().any(|l| l.exclusive_scope().is_some()) {
        Some(list_on_issue(client, owner, name, number).await?)
    } else {
        None
    };
    let ids = requested.iter().map(|l| l.id).collect();
    let after = add_to_issue(client, owner, name, number, ids).await?;
    Ok(Assignment {
        before,
        requested,
        after,
    })
}

pub async fn remove_from_issue(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    label_id: u64,
) -> Result<()> {
    let path = format!("{}/{label_id}", issue_labels_path(owner, name, number));
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}
