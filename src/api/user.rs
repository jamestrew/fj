use anyhow::Result;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// Signed because Forgejo issues negative ids to its system actors (the
    /// Ghost/deleted-user placeholder is -1 and the `forgejo-actions` bot is
    /// -2). Those users appear in real API responses (e.g. as a requested
    /// reviewer), so a `u64` here rejects the whole response with a decode
    /// error. Never used as an unsigned quantity, so widening to `i64` is safe.
    pub id: i64,
    pub login: String,
    #[serde(default)]
    pub full_name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub avatar_url: String,
    #[serde(default)]
    pub is_admin: bool,
}

pub async fn current(client: &Client) -> Result<User> {
    client
        .json(Method::GET, "/api/v1/user", &[], None::<&()>)
        .await
}
