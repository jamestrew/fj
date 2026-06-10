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
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct EditLabel<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
}

pub async fn list(client: &Client, owner: &str, name: &str) -> Result<Vec<Label>> {
    let path = format!("{}/labels", crate::api::repo_path(owner, name));
    client.json(Method::GET, &path, &[], None::<&()>).await
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

pub async fn resolve_names_to_ids(
    client: &Client,
    owner: &str,
    name: &str,
    label_names: &[String],
) -> Result<Vec<u64>> {
    let labels = list(client, owner, name).await?;
    let mut ids = Vec::with_capacity(label_names.len());
    for label_name in label_names {
        let id = labels
            .iter()
            .find(|l| l.name == *label_name)
            .map(|l| l.id)
            .ok_or_else(|| anyhow!("no label named '{label_name}'"))?;
        ids.push(id);
    }
    Ok(ids)
}

pub async fn create(
    client: &Client,
    owner: &str,
    name: &str,
    body: &CreateLabel<'_>,
) -> Result<Label> {
    let path = format!("{}/labels", crate::api::repo_path(owner, name));
    client.json(Method::POST, &path, &[], Some(body)).await
}

pub async fn edit(
    client: &Client,
    owner: &str,
    name: &str,
    id: u64,
    body: &EditLabel<'_>,
) -> Result<Label> {
    let path = format!("{}/labels/{id}", crate::api::repo_path(owner, name));
    client.json(Method::PATCH, &path, &[], Some(body)).await
}

pub async fn delete(client: &Client, owner: &str, name: &str, id: u64) -> Result<()> {
    let path = format!("{}/labels/{id}", crate::api::repo_path(owner, name));
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct AddLabelsBody {
    pub labels: Vec<u64>,
}

#[allow(dead_code)]
pub async fn add_to_issue(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    label_ids: Vec<u64>,
) -> Result<Vec<Label>> {
    let path = format!(
        "{}/issues/{number}/labels",
        crate::api::repo_path(owner, name)
    );
    client
        .json(
            Method::POST,
            &path,
            &[],
            Some(&AddLabelsBody { labels: label_ids }),
        )
        .await
}

#[allow(dead_code)]
pub async fn remove_from_issue(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    label_id: u64,
) -> Result<()> {
    let path = format!(
        "{}/issues/{number}/labels/{label_id}",
        crate::api::repo_path(owner, name)
    );
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}
