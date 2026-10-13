//! Actions runners. For now, the registration token a new runner presents to
//! `forgejo-runner register`.

use anyhow::Result;
use reqwest::Method;
use serde::Deserialize;

use crate::client::Client;

/// Where a runner registers, which decides what it may pick up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Repo {
        owner: String,
        name: String,
    },
    Org(String),
    /// The signed-in account's own repositories.
    User,
    /// Every repository on the instance. Needs an instance administrator.
    Instance,
}

impl Scope {
    fn token_path(&self) -> String {
        const LEAF: &str = "actions/runners/registration-token";
        match self {
            Scope::Repo { owner, name } => {
                format!("{}/{LEAF}", crate::api::repo_path(owner, name))
            }
            Scope::Org(org) => format!("/api/v1/orgs/{}/{LEAF}", crate::api::path_segment(org)),
            Scope::User => format!("/api/v1/user/{LEAF}"),
            // The un-prefixed admin route is the one Forgejo and Gitea share.
            Scope::Instance => "/api/v1/admin/runners/registration-token".to_string(),
        }
    }
}

#[derive(Deserialize)]
struct RegistrationToken {
    token: String,
}

/// Fetch a runner registration token. The forge serves this as a GET (a POST
/// is a 405 on Forgejo).
pub async fn registration_token(client: &Client, scope: &Scope) -> Result<String> {
    let reply: RegistrationToken = client
        .json(Method::GET, &scope.token_path(), &[], None::<&()>)
        .await?;
    if reply.token.is_empty() {
        anyhow::bail!("the server answered without a registration token");
    }
    Ok(reply.token)
}
