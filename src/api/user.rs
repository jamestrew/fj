use anyhow::Result;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// Signed: Forgejo uses negative sentinel ids for system actors
    /// (-1 Ghost/deleted, -2 the `forgejo-actions` bot), which appear as
    /// reviewers, assignees, posters, and authors.
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

#[cfg(test)]
mod tests {
    use super::User;

    #[test]
    fn deserializes_negative_system_actor_id() {
        // Forgejo returns id -2 for the `forgejo-actions` bot (and -1 for
        // Ghost). These appear in requested_reviewers/assignees/poster, so the
        // User id must decode as signed or `fj pr list`/`view` fails wholesale.
        let user: User = serde_json::from_str(r#"{"id":-2,"login":"forgejo-actions"}"#).unwrap();
        assert_eq!(user.id, -2);
        assert_eq!(user.login, "forgejo-actions");
    }
}
