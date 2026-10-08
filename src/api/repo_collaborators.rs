//! Repository collaborator operations: list, add/update, and remove.

use anyhow::{anyhow, Result};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::client::{ApiError, Client, Page};

use super::user::User;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collaborator {
    #[serde(flatten)]
    pub user: User,
    /// The flags the list endpoint sends. Forgejo returns plain user objects
    /// there with `permissions: null`, so this is usually `None` and is never
    /// read as "no access".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<RepoPermissions>,
    /// Access confirmed by `GET .../collaborators/{user}/permission`
    /// (`read`, `write`, `admin` or `owner`). `None` until looked up, or when
    /// the forge would not disclose it.
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_name: Option<String>,
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
    /// The collaborator's access: the confirmed permission when it was looked
    /// up, else the list endpoint's flags when it sent any, else `unknown`.
    /// Absent data is never reported as `none`.
    pub fn permission_level(&self) -> &str {
        if let Some(permission) = self.permission.as_deref().filter(|p| !p.is_empty()) {
            return permission;
        }
        match &self.permissions {
            Some(p) if p.admin => "admin",
            Some(p) if p.push => "write",
            Some(p) if p.pull => "read",
            Some(_) => "none",
            None => "unknown",
        }
    }
}

/// `RepoCollaboratorPermission`: one user's confirmed access to a repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaboratorAccess {
    #[serde(default)]
    pub permission: String,
    #[serde(default)]
    pub role_name: String,
    #[serde(default)]
    pub user: Option<User>,
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

/// Collaborators, following pagination up to `limit` rows (all when `None`).
pub async fn list_collaborators_page(
    client: &Client,
    owner: &str,
    name: &str,
    limit: Option<u32>,
) -> Result<Page<Collaborator>> {
    let path = format!("{}/collaborators", crate::api::repo_path(owner, name));
    client
        .get_all_page(&path, &[], limit.map(|l| l as usize))
        .await
}

/// The access the forge confirms for `user` on this repository.
pub async fn collaborator_permission(
    client: &Client,
    owner: &str,
    name: &str,
    user: &str,
) -> Result<CollaboratorAccess> {
    let path = format!("{}/permission", collaborator_path(owner, name, user));
    client.json(Method::GET, &path, &[], None::<&()>).await
}

/// Fill in each collaborator's confirmed permission. The list endpoint cannot
/// answer that, so it takes one lookup per row. A forge that will not disclose
/// another user's permission to this account answers 403: that row is left
/// unresolved (shown as `unknown`) and counted in the return value. Any other
/// failure is an error.
pub async fn resolve_collaborator_permissions(
    client: &Client,
    owner: &str,
    name: &str,
    collaborators: &mut [Collaborator],
) -> Result<usize> {
    let mut undisclosed = 0;
    for c in collaborators.iter_mut() {
        match collaborator_permission(client, owner, name, &c.user.login).await {
            Ok(access) => {
                c.permission = Some(access.permission).filter(|p| !p.is_empty());
                c.role_name = Some(access.role_name).filter(|r| !r.is_empty());
            }
            Err(err)
                if err
                    .downcast_ref::<ApiError>()
                    .is_some_and(|e| e.status == StatusCode::FORBIDDEN) =>
            {
                undisclosed += 1;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(undisclosed)
}

/// 204 means `user` is a collaborator, 404 means not.
pub async fn is_collaborator(client: &Client, owner: &str, name: &str, user: &str) -> Result<bool> {
    let path = collaborator_path(owner, name, user);
    let res = client.request(Method::GET, &path, &[], None).await?;
    match res.status() {
        StatusCode::NOT_FOUND => Ok(false),
        _ => crate::client::ensure_success(res).await.map(|_| true),
    }
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
    client.send(Method::PUT, &path, &[], Some(&body)).await?;
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
        StatusCode::FORBIDDEN => Err(anyhow!(
            "permission denied: removing a collaborator from {owner}/{name} needs admin access to it: {message}"
        )),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn collaborator(v: serde_json::Value) -> Collaborator {
        serde_json::from_value(v).expect("collaborator")
    }

    #[test]
    fn null_permissions_are_unknown_not_none() {
        // fj#276: Forgejo's list sends `permissions: null`. That used to
        // default to all-false and print "none" for every collaborator.
        let c = collaborator(serde_json::json!({ "id": 1, "login": "alice", "permissions": null }));
        assert!(c.permissions.is_none());
        assert_eq!(c.permission_level(), "unknown");
        let c = collaborator(serde_json::json!({ "id": 1, "login": "alice" }));
        assert_eq!(c.permission_level(), "unknown");
    }

    #[test]
    fn confirmed_permission_wins_over_list_flags() {
        let mut c = collaborator(serde_json::json!({
            "id": 1, "login": "alice",
            "permissions": { "admin": false, "push": false, "pull": true }
        }));
        assert_eq!(c.permission_level(), "read");
        c.permission = Some("admin".into());
        assert_eq!(c.permission_level(), "admin");
    }

    #[test]
    fn explicit_all_false_flags_still_read_as_none() {
        let c = collaborator(serde_json::json!({
            "id": 1, "login": "alice",
            "permissions": { "admin": false, "push": false, "pull": false }
        }));
        assert_eq!(c.permission_level(), "none");
    }
}
