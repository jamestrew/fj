pub mod issue;
pub mod pull;
pub mod repo;
pub mod user;

use anyhow::{anyhow, Result};

/// Split an `owner/name` slug. Returns a helpful error if the form is wrong.
pub fn split_repo(repo: &str) -> Result<(&str, &str)> {
    repo.split_once('/')
        .filter(|(o, n)| !o.is_empty() && !n.is_empty())
        .ok_or_else(|| anyhow!("expected '<owner>/<name>', got '{repo}'"))
}
