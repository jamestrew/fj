//! Repository collaborator operations: list, add/update, and remove.

use anyhow::{anyhow, Result};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::client::Client;

use super::user::User;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collaborator {
    #[serde(flatten)]
    pub user: User,
    #[serde(default)]
    pub permissions: RepoPermissions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoPermissions {
    #[serde(default)]
    pub admin: bool,
    #[serde(default)]
    pub push: bool,
    #[serde(default)]
    pub pull: bool,
}

impl Collaborator {
    pub fn permission_level(&self) -> &'static str {
        if self.permissions.admin {
            "admin"
        } else if self.permissions.push {
            "write"
        } else if self.permissions.pull {
            "read"
        } else {
            "none"
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum CollaboratorPermission {
    Read,
    Write,
    Admin,
}

impl CollaboratorPermission {
    pub fn as_api_value(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }
}

#[derive(Debug, Serialize)]
struct CollaboratorBody<'a> {
    permission: &'a str,
}

pub async fn list_collaborators(
    client: &Client,
    owner: &str,
    name: &str,
) -> Result<Vec<Collaborator>> {
    let path = format!("{}/collaborators", crate::api::repo_path(owner, name));
    client.json(Method::GET, &path, &[], None::<&()>).await
}

pub async fn add_collaborator(
    client: &Client,
    owner: &str,
    name: &str,
    user: &str,
    permission: CollaboratorPermission,
) -> Result<()> {
    let path = collaborator_path(owner, name, user);
    let body = CollaboratorBody {
        permission: permission.as_api_value(),
    };
    let res = client
        .request(Method::PUT, &path, &[], Some(&serde_json::to_value(body)?))
        .await?;
    res.error_for_status()?;
    Ok(())
}

pub async fn remove_collaborator(
    client: &Client,
    owner: &str,
    name: &str,
    user: &str,
) -> Result<()> {
    let path = collaborator_path(owner, name, user);
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    let status = res.status();
    if status.is_success() {
        return Ok(());
    }
    let message = api_error_message(res).await;
    match status {
        StatusCode::NOT_FOUND => Err(anyhow!("'{user}' is not a collaborator on {owner}/{name}")),
        StatusCode::UNPROCESSABLE_ENTITY if message.to_lowercase().contains("collaborator") => {
            Err(anyhow!("'{user}' is not a collaborator on {owner}/{name}"))
        }
        _ => Err(anyhow!(
            "could not remove collaborator '{user}': HTTP {status}: {message}"
        )),
    }
}

fn collaborator_path(owner: &str, name: &str, user: &str) -> String {
    format!(
        "{}/collaborators/{}",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(user)
    )
}

async fn api_error_message(res: reqwest::Response) -> String {
    let text = res.text().await.unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| {
            v.get("message")
                .or_else(|| v.get("error"))
                .and_then(|m| m.as_str())
                .map(ToString::to_string)
        })
        .filter(|s| !s.is_empty())
        .unwrap_or(text)
}
