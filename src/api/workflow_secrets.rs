//! Forgejo Actions secrets and variables (`/api/v1/repos/.../actions/...`).

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
    let path = format!("{}/actions/secrets", crate::api::repo_path(owner, name));
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        secrets: Vec<ActionSecret>,
    }
    let env: Envelope = client.json(Method::GET, &path, &[], None::<&()>).await?;
    Ok(env.secrets)
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
    let path = format!(
        "{}/actions/secrets/{}",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(secret_name)
    );
    let res = client
        .request(
            Method::PUT,
            &path,
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
    let path = format!(
        "{}/actions/secrets/{}",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(secret_name)
    );
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionVariable {
    pub name: String,
    pub data: String,
}

pub async fn list_variables(
    client: &Client,
    owner: &str,
    name: &str,
) -> Result<Vec<ActionVariable>> {
    let path = format!("{}/actions/variables", crate::api::repo_path(owner, name));
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        variables: Vec<ActionVariable>,
    }
    let env: Envelope = client.json(Method::GET, &path, &[], None::<&()>).await?;
    Ok(env.variables)
}

pub async fn set_variable(
    client: &Client,
    owner: &str,
    name: &str,
    var_name: &str,
    value: &str,
) -> Result<()> {
    let path = format!(
        "{}/actions/variables/{}",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(var_name)
    );
    let res = client
        .request(
            Method::PUT,
            &path,
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
    let path = format!(
        "{}/actions/variables/{}",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(var_name)
    );
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}
