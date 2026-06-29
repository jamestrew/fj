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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionVariable {
    pub name: String,
    pub created_at: DateTime<Utc>,
}

pub async fn list_variables(
    client: &Client,
    owner: &str,
    name: &str,
) -> Result<Vec<ActionVariable>> {
    list_variables_at(client, &repo_actions_path(owner, name, "variables")).await
}

pub async fn list_org_variables(client: &Client, org: &str) -> Result<Vec<ActionVariable>> {
    list_variables_at(client, &org_actions_path(org, "variables")).await
}

async fn list_variables_at(client: &Client, path: &str) -> Result<Vec<ActionVariable>> {
    client.json(Method::GET, path, &[], None::<&()>).await
}

pub async fn set_variable(
    client: &Client,
    owner: &str,
    name: &str,
    var_name: &str,
    value: &str,
) -> Result<()> {
    set_variable_at(
        client,
        &repo_actions_item_path(owner, name, "variables", var_name),
        value,
    )
    .await
}

pub async fn set_org_variable(
    client: &Client,
    org: &str,
    var_name: &str,
    value: &str,
) -> Result<()> {
    set_variable_at(
        client,
        &org_actions_item_path(org, "variables", var_name),
        value,
    )
    .await
}

async fn set_variable_at(client: &Client, path: &str, value: &str) -> Result<()> {
    let res = client
        .request(
            Method::PUT,
            path,
            &[],
            Some(&serde_json::json!({ "value": value })),
        )
        .await?;
    res.error_for_status()?;
    Ok(())
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
    )
    .await
}

pub async fn delete_org_variable(client: &Client, org: &str, var_name: &str) -> Result<()> {
    delete_variable_at(client, &org_actions_item_path(org, "variables", var_name)).await
}

async fn delete_variable_at(client: &Client, path: &str) -> Result<()> {
    let res = client.request(Method::DELETE, path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
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
    fn action_variables_decode_live_bare_array_without_values() {
        let vars: Vec<ActionVariable> = serde_json::from_value(serde_json::json!([
            { "name": "RUST_LOG", "created_at": "2026-01-01T00:00:00Z" }
        ]))
        .expect("bare variable array");

        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].name, "RUST_LOG");
        let rendered = serde_json::to_string(&vars).unwrap();
        assert!(!rendered.contains("variable-value"));
    }
}
