//! Forgejo Actions secrets and variables (`/api/v1/repos/.../actions/...`
//! and `/api/v1/orgs/.../actions/...`).

use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionSecret {
    pub name: String,
    pub created_at: DateTime<Utc>,
}

pub async fn list_secrets(client: &Client, owner: &str, name: &str) -> Result<Vec<ActionSecret>> {
    list_secrets_at(client, &repo_actions_path(owner, name, "secrets")).await
}

pub async fn list_org_secrets(client: &Client, org: &str) -> Result<Vec<ActionSecret>> {
    list_secrets_at(client, &org_actions_path(org, "secrets")).await
}

async fn list_secrets_at(client: &Client, path: &str) -> Result<Vec<ActionSecret>> {
    client.json(Method::GET, path, &[], None::<&()>).await
}

#[derive(Debug, Serialize)]
struct SecretBody<'a> {
    data: &'a str,
}

pub async fn set_secret(
    client: &Client,
    owner: &str,
    name: &str,
    secret_name: &str,
    value: &str,
) -> Result<()> {
    set_secret_at(
        client,
        &repo_actions_item_path(owner, name, "secrets", secret_name),
        value,
    )
    .await
}

pub async fn set_org_secret(
    client: &Client,
    org: &str,
    secret_name: &str,
    value: &str,
) -> Result<()> {
    set_secret_at(
        client,
        &org_actions_item_path(org, "secrets", secret_name),
        value,
    )
    .await
}

async fn set_secret_at(client: &Client, path: &str, value: &str) -> Result<()> {
    let res = client
        .request(
            Method::PUT,
            path,
            &[],
            Some(&serde_json::to_value(SecretBody { data: value })?),
        )
        .await?;
    res.error_for_status()?;
    Ok(())
}

pub async fn delete_secret(
    client: &Client,
    owner: &str,
    name: &str,
    secret_name: &str,
) -> Result<()> {
    delete_secret_at(
        client,
        &repo_actions_item_path(owner, name, "secrets", secret_name),
    )
    .await
}

pub async fn delete_org_secret(client: &Client, org: &str, secret_name: &str) -> Result<()> {
    delete_secret_at(client, &org_actions_item_path(org, "secrets", secret_name)).await
}

async fn delete_secret_at(client: &Client, path: &str) -> Result<()> {
    let res = client.request(Method::DELETE, path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

/// An Actions variable as the server reports it. Unlike a secret, a variable
/// is plain configuration: its value (`data`) is readable by anyone who can
/// read the list, so it is carried and shown.
///
/// `owner_id` / `repo_id` are the server's own record of where the variable
/// lives (see [`ActionVariable::scope`]). `created_at` is not part of the
/// Forgejo wire shape; it is tolerated for servers that add it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionVariable {
    pub name: String,
    #[serde(default)]
    pub data: String,
    #[serde(default)]
    pub owner_id: i64,
    #[serde(default)]
    pub repo_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

/// Where a variable's value applies, as recorded by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariableScope {
    /// Bound to one repository (`repo_id` set).
    Repository,
    /// Bound to an owner (organization or user) and shared by its
    /// repositories (`owner_id` set, no `repo_id`).
    Owner,
    /// The server reported neither id, so the scope is not known.
    Unreported,
}

impl ActionVariable {
    pub fn scope(&self) -> VariableScope {
        if self.repo_id != 0 {
            VariableScope::Repository
        } else if self.owner_id != 0 {
            VariableScope::Owner
        } else {
            VariableScope::Unreported
        }
    }
}

/// Whether a `set` created the variable or replaced an existing value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariableWrite {
    Created,
    Updated,
}

/// Result of a `set`: what happened, plus the variable re-read from the
/// server afterwards (the name the server stored and the scope it recorded).
#[derive(Debug, Clone)]
pub struct VariableOutcome {
    pub write: VariableWrite,
    pub variable: ActionVariable,
}

pub async fn list_variables(
    client: &Client,
    owner: &str,
    name: &str,
) -> Result<Vec<ActionVariable>> {
    list_variables_at(client, &repo_actions_path(owner, name, "variables"))
        .await
        .map_err(|e| variables_unavailable(e, &format!("{owner}/{name}")))
}

pub async fn list_org_variables(client: &Client, org: &str) -> Result<Vec<ActionVariable>> {
    list_variables_at(client, &org_actions_path(org, "variables"))
        .await
        .map_err(|e| variables_unavailable(e, &format!("organization {org}")))
}

async fn list_variables_at(client: &Client, path: &str) -> Result<Vec<ActionVariable>> {
    Ok(client.get_all_page(path, &[], None).await?.items)
}

/// Create the variable, or replace its value when it already exists, then
/// re-read it so the caller reports what the server stored.
pub async fn set_variable(
    client: &Client,
    owner: &str,
    name: &str,
    var_name: &str,
    value: &str,
) -> Result<VariableOutcome> {
    set_variable_at(
        client,
        &repo_actions_item_path(owner, name, "variables", var_name),
        value,
    )
    .await
    .map_err(|e| variables_unavailable(e, &format!("{owner}/{name}")))
}

pub async fn set_org_variable(
    client: &Client,
    org: &str,
    var_name: &str,
    value: &str,
) -> Result<VariableOutcome> {
    set_variable_at(
        client,
        &org_actions_item_path(org, "variables", var_name),
        value,
    )
    .await
    .map_err(|e| variables_unavailable(e, &format!("organization {org}")))
}

/// Forgejo splits the write: POST creates (and rejects an existing name),
/// PUT updates (and 404s on a missing one). Look first, then pick. Neither
/// verb's response carries the variable, so a final GET confirms it.
async fn set_variable_at(client: &Client, path: &str, value: &str) -> Result<VariableOutcome> {
    let body = serde_json::json!({ "value": value });
    let (method, write) = match get_variable_at(client, path).await? {
        Some(_) => (Method::PUT, VariableWrite::Updated),
        None => (Method::POST, VariableWrite::Created),
    };
    let res = client.request(method, path, &[], Some(&body)).await?;
    crate::client::ensure_success(res).await?;
    let variable = get_variable_at(client, path).await?.ok_or_else(|| {
        anyhow::anyhow!("the server accepted the variable but does not report it back")
    })?;
    Ok(VariableOutcome { write, variable })
}

/// One variable, or `None` when the server has no variable by that name.
async fn get_variable_at(client: &Client, path: &str) -> Result<Option<ActionVariable>> {
    let res = client.request(Method::GET, path, &[], None).await?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let res = crate::client::ensure_success(res).await?;
    Ok(Some(res.json().await?))
}

pub async fn delete_variable(
    client: &Client,
    owner: &str,
    name: &str,
    var_name: &str,
) -> Result<()> {
    delete_variable_at(
        client,
        &repo_actions_item_path(owner, name, "variables", var_name),
        var_name,
    )
    .await
}

pub async fn delete_org_variable(client: &Client, org: &str, var_name: &str) -> Result<()> {
    delete_variable_at(
        client,
        &org_actions_item_path(org, "variables", var_name),
        var_name,
    )
    .await
}

async fn delete_variable_at(client: &Client, path: &str, var_name: &str) -> Result<()> {
    let res = client.request(Method::DELETE, path, &[], None).await?;
    crate::client::ensure_success(res)
        .await
        .map_err(|e| crate::client::with_not_found_context(e, variable_missing(var_name)))?;
    Ok(())
}

fn variable_missing(var_name: &str) -> String {
    format!(
        "variable '{var_name}' not found (it may already be deleted, or Actions is disabled here)"
    )
}

/// A 404 on the variables collection, or on creating into it, is not "no
/// variables": the scope itself is missing or has no Actions. Say so, apart
/// from the 403 that `ensure_success` already words as a permission problem.
fn variables_unavailable(err: anyhow::Error, scope: &str) -> anyhow::Error {
    crate::client::with_not_found_context(
        err,
        format!(
            "Actions variables are unavailable for {scope}: it was not found, \
             Actions is disabled there, or this server has no variables API"
        ),
    )
}

fn repo_actions_path(owner: &str, name: &str, kind: &str) -> String {
    format!("{}/actions/{kind}", crate::api::repo_path(owner, name))
}

fn repo_actions_item_path(owner: &str, name: &str, kind: &str, item: &str) -> String {
    format!(
        "{}/{}",
        repo_actions_path(owner, name, kind),
        crate::api::path_segment(item)
    )
}

fn org_actions_path(org: &str, kind: &str) -> String {
    format!(
        "/api/v1/orgs/{}/actions/{kind}",
        crate::api::path_segment(org)
    )
}

fn org_actions_item_path(org: &str, kind: &str, item: &str) -> String {
    format!(
        "{}/{}",
        org_actions_path(org, kind),
        crate::api::path_segment(item)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_secrets_decode_live_bare_array_without_values() {
        let secrets: Vec<ActionSecret> = serde_json::from_value(serde_json::json!([
            { "name": "TOKEN", "created_at": "2026-01-01T00:00:00Z" }
        ]))
        .expect("bare secret array");

        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].name, "TOKEN");
        let rendered = serde_json::to_string(&secrets).unwrap();
        assert!(!rendered.contains("secret-value"));
    }

    #[test]
    fn action_variables_decode_the_forgejo_wire_shape() {
        // Forgejo's ActionVariable is {name, data, owner_id, repo_id}: there
        // is no created_at, so requiring one fails every non-empty list.
        let vars: Vec<ActionVariable> = serde_json::from_value(serde_json::json!([
            { "name": "RUST_LOG", "data": "debug", "owner_id": 3, "repo_id": 9 }
        ]))
        .expect("bare variable array");

        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].name, "RUST_LOG");
        assert_eq!(vars[0].data, "debug");
        assert_eq!(vars[0].scope(), VariableScope::Repository);
        assert!(vars[0].created_at.is_none());
        let rendered = serde_json::to_value(&vars).unwrap();
        assert!(rendered[0].get("created_at").is_none());
    }

    #[test]
    fn action_variable_tolerates_a_created_at_and_missing_ids() {
        let var: ActionVariable = serde_json::from_value(serde_json::json!(
            { "name": "RUST_LOG", "created_at": "2026-01-01T00:00:00Z" }
        ))
        .expect("sparse variable");
        assert!(var.created_at.is_some());
        assert_eq!(var.data, "");
        assert_eq!(var.scope(), VariableScope::Unreported);
    }

    #[test]
    fn variable_scope_comes_from_the_server_ids() {
        let v = |owner_id, repo_id| ActionVariable {
            name: "X".into(),
            data: String::new(),
            owner_id,
            repo_id,
            created_at: None,
        };
        assert_eq!(v(0, 7).scope(), VariableScope::Repository);
        // A repo variable can carry its owner id too; the repo binding wins.
        assert_eq!(v(4, 7).scope(), VariableScope::Repository);
        assert_eq!(v(4, 0).scope(), VariableScope::Owner);
        assert_eq!(v(0, 0).scope(), VariableScope::Unreported);
    }

    #[test]
    fn missing_variable_and_unavailable_scope_read_differently() {
        assert!(variable_missing("REGION").contains("variable 'REGION' not found"));
        let not_found = anyhow::Error::new(crate::client::ApiError {
            status: reqwest::StatusCode::NOT_FOUND,
            url: "https://forge.example/api/v1/repos/o/r/actions/variables".into(),
            message: String::new(),
            body: String::new(),
        });
        let msg = variables_unavailable(not_found, "o/r").to_string();
        assert!(msg.contains("Actions variables are unavailable for o/r"));
        assert!(msg.contains("Actions is disabled"));

        // A permission failure keeps its own wording.
        let forbidden = anyhow::Error::new(crate::client::ApiError {
            status: reqwest::StatusCode::FORBIDDEN,
            url: "https://forge.example/api/v1/repos/o/r/actions/variables".into(),
            message: String::new(),
            body: String::new(),
        })
        .context("forbidden: o/r (token lacks scope or access)");
        let msg = variables_unavailable(forbidden, "o/r").to_string();
        assert!(msg.contains("forbidden"));
        assert!(!msg.contains("unavailable"));
    }
}
