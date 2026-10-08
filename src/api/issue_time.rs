//! Tracked time on an issue or pull request (they share the issue index).

use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

/// One time entry as the forge holds it. The response also embeds the whole
/// issue; we leave that out so `--json` stays about the entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedTime {
    pub id: u64,
    /// Duration in seconds.
    #[serde(default)]
    pub time: u64,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub created: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
struct AddTime {
    time: u64,
}

fn path(owner: &str, name: &str, index: u64) -> String {
    format!(
        "{}/issues/{index}/times",
        crate::api::repo_path(owner, name)
    )
}

/// Every entry on the work item, so a total summed from it is the forge's.
pub async fn list(
    client: &Client,
    owner: &str,
    name: &str,
    index: u64,
) -> Result<Vec<TrackedTime>> {
    let page = client
        .get_all_page::<TrackedTime>(&path(owner, name, index), &[], None)
        .await?;
    Ok(page.items)
}

pub async fn add(
    client: &Client,
    owner: &str,
    name: &str,
    index: u64,
    seconds: u64,
) -> Result<TrackedTime> {
    client
        .json(
            Method::POST,
            &path(owner, name, index),
            &[],
            Some(&AddTime { time: seconds }),
        )
        .await
}

pub async fn delete(client: &Client, owner: &str, name: &str, index: u64, id: u64) -> Result<()> {
    let path = format!("{}/{id}", path(owner, name, index));
    client.send(Method::DELETE, &path, &[], None::<&()>).await?;
    Ok(())
}
