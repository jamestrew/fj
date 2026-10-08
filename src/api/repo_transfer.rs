//! Repository ownership transfers: request, inspect, accept and reject.
//!
//! Forgejo has no endpoint that lists pending transfers. A pending transfer
//! is only visible as the `repo_transfer` field of the repository it applies
//! to, so discovery means reading repositories and keeping the ones that
//! carry it.

use anyhow::Result;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::client::Client;

use super::serde_util::deserialize_null_to_default;
use super::user::User;

/// A team attached to a pending transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferTeam {
    pub id: u64,
    #[serde(default)]
    pub name: String,
}

/// `RepoTransfer`: a pending transfer, as carried on the repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoTransfer {
    /// Who requested the transfer.
    #[serde(default)]
    pub doer: Option<User>,
    /// The receiving owner. The API describes users and organizations with
    /// the same shape here, so this alone does not say which it is.
    #[serde(default)]
    pub recipient: Option<User>,
    #[serde(default, deserialize_with = "deserialize_null_to_default")]
    pub teams: Vec<TransferTeam>,
}

/// The slice of a `Repository` the transfer commands read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferRepo {
    pub name: String,
    pub full_name: String,
    pub owner: User,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub repo_transfer: Option<RepoTransfer>,
}

pub async fn transfer_state(client: &Client, owner: &str, name: &str) -> Result<TransferRepo> {
    let path = crate::api::repo_path(owner, name);
    client.json(Method::GET, &path, &[], None::<&()>).await
}

#[derive(Debug, Serialize)]
struct TransferBody<'a> {
    new_owner: &'a str,
    #[serde(skip_serializing_if = "<[u64]>::is_empty")]
    team_ids: &'a [u64],
}

/// `POST .../transfer`. The returned repository is the server's verdict: it
/// either has the new owner already or carries the pending `repo_transfer`.
pub async fn request_transfer(
    client: &Client,
    owner: &str,
    name: &str,
    new_owner: &str,
    team_ids: &[u64],
) -> Result<TransferRepo> {
    let path = format!("{}/transfer", crate::api::repo_path(owner, name));
    let body = TransferBody {
        new_owner,
        team_ids,
    };
    client.json(Method::POST, &path, &[], Some(&body)).await
}

pub async fn accept_transfer(client: &Client, owner: &str, name: &str) -> Result<TransferRepo> {
    let path = format!("{}/transfer/accept", crate::api::repo_path(owner, name));
    client.json(Method::POST, &path, &[], None::<&()>).await
}

pub async fn reject_transfer(client: &Client, owner: &str, name: &str) -> Result<TransferRepo> {
    let path = format!("{}/transfer/reject", crate::api::repo_path(owner, name));
    client.json(Method::POST, &path, &[], None::<&()>).await
}

/// Repositories that carry a pending transfer, out of one listing: the
/// signed-in account's repositories (`GET /user/repos`), or with `source` the
/// repositories of that user or organization the account can see
/// (`GET /users/{source}/repos`). Every page of the listing is read.
pub async fn list_pending_transfers(
    client: &Client,
    source: Option<&str>,
) -> Result<Vec<TransferRepo>> {
    let path = match source {
        Some(owner) => format!("/api/v1/users/{}/repos", crate::api::path_segment(owner)),
        None => "/api/v1/user/repos".to_string(),
    };
    let repos: Vec<TransferRepo> = client.get_all_page(&path, &[], None).await?.items;
    Ok(repos
        .into_iter()
        .filter(|r| r.repo_transfer.is_some())
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerKind {
    User,
    Organization,
}

impl OwnerKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Organization => "organization",
        }
    }
}

/// An account found on the forge, with the name spelled as the forge has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerLookup {
    pub kind: OwnerKind,
    pub login: String,
    pub full_name: String,
}

#[derive(Deserialize)]
struct OrgName {
    #[serde(default)]
    name: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    full_name: String,
}

/// Find `login` on the forge and say whether it is a user or an organization.
/// `GET /orgs/{login}` answers 200 only for an organization; a 404 there falls
/// back to `GET /users/{login}`. `None` means neither exists, or it is an
/// organization this account is not allowed to see.
pub async fn lookup_owner(client: &Client, login: &str) -> Result<Option<OwnerLookup>> {
    let org_path = format!("/api/v1/orgs/{}", crate::api::path_segment(login));
    let res = client.request(Method::GET, &org_path, &[], None).await?;
    if res.status() != StatusCode::NOT_FOUND {
        let org: OrgName = crate::client::ensure_success(res).await?.json().await?;
        let name = if org.name.is_empty() {
            org.username
        } else {
            org.name
        };
        return Ok(Some(OwnerLookup {
            kind: OwnerKind::Organization,
            login: if name.is_empty() {
                login.to_string()
            } else {
                name
            },
            full_name: org.full_name,
        }));
    }
    let user_path = format!("/api/v1/users/{}", crate::api::path_segment(login));
    let res = client.request(Method::GET, &user_path, &[], None).await?;
    if res.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let user: User = crate::client::ensure_success(res).await?.json().await?;
    Ok(Some(OwnerLookup {
        kind: OwnerKind::User,
        login: user.login,
        full_name: user.full_name,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_without_transfer_decodes_to_none() {
        let r: TransferRepo = serde_json::from_value(serde_json::json!({
            "name": "r", "full_name": "o/r", "owner": { "id": 1, "login": "o" },
            "repo_transfer": null
        }))
        .unwrap();
        assert!(r.repo_transfer.is_none());
    }

    #[test]
    fn pending_transfer_decodes_with_null_teams() {
        let r: TransferRepo = serde_json::from_value(serde_json::json!({
            "name": "r", "full_name": "o/r", "owner": { "id": 1, "login": "o" },
            "repo_transfer": {
                "doer": { "id": 1, "login": "o" },
                "recipient": { "id": 2, "login": "acme" },
                "teams": null
            }
        }))
        .unwrap();
        let t = r.repo_transfer.expect("pending");
        assert_eq!(t.recipient.unwrap().login, "acme");
        assert!(t.teams.is_empty());
    }

    #[test]
    fn transfer_body_omits_empty_team_ids() {
        let body = TransferBody {
            new_owner: "acme",
            team_ids: &[],
        };
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({ "new_owner": "acme" })
        );
        let body = TransferBody {
            new_owner: "acme",
            team_ids: &[3, 4],
        };
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({ "new_owner": "acme", "team_ids": [3, 4] })
        );
    }
}
