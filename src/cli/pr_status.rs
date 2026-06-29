//! `fj pr status` — a cross-repo dashboard of your open PRs: ones you
//! authored, ones requesting your review, and ones that mention you. Unlike
//! the other `pr` subcommands this is not scoped to a single repo, so it
//! connects by host rather than resolving a repo from `-R`/the git remote.

use anyhow::Result;
use clap::Args;

use crate::api;
use crate::client::Client;
use crate::output;

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Maximum number of pull requests to fetch
    #[arg(short = 'L', long, default_value_t = 20)]
    pub limit: u32,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

pub async fn status(args: StatusArgs, host: Option<&str>) -> Result<()> {
    // Cross-repo dashboard: PRs authored by you, and PRs where you're listed
    // as a reviewer or where you're mentioned.
    let client = Client::connect(host)?;
    let me = api::user::current(&client).await?;
    let limit = args.limit.clamp(1, 50);

    let q_authored: Vec<(String, String)> = vec![
        ("type".into(), "pulls".into()),
        ("state".into(), "open".into()),
        ("created_by".into(), me.login.clone()),
        ("limit".into(), limit.to_string()),
    ];
    let q_review: Vec<(String, String)> = vec![
        ("type".into(), "pulls".into()),
        ("state".into(), "open".into()),
        ("reviewed_by".into(), me.login.clone()),
        ("limit".into(), limit.to_string()),
    ];
    let q_mentioned: Vec<(String, String)> = vec![
        ("type".into(), "pulls".into()),
        ("state".into(), "open".into()),
        ("mentioned_by".into(), me.login.clone()),
        ("limit".into(), limit.to_string()),
    ];

    let path = "/api/v1/repos/issues/search";
    let authored: Vec<api::issue::Issue> = client
        .json(reqwest::Method::GET, path, &q_authored, None::<&()>)
        .await?;
    let review_requested: Vec<api::issue::Issue> = client
        .json(reqwest::Method::GET, path, &q_review, None::<&()>)
        .await?;
    let mentioned: Vec<api::issue::Issue> = client
        .json(reqwest::Method::GET, path, &q_mentioned, None::<&()>)
        .await?;

    if args.json {
        return output::print_json(&status_json_value(&authored, &review_requested, &mentioned));
    }

    print_status_section("Created by you", &authored);
    print_status_section("Requesting your review", &review_requested);
    print_status_section("Mentioning you", &mentioned);
    Ok(())
}

fn status_json_value(
    authored: &[api::issue::Issue],
    review_requested: &[api::issue::Issue],
    mentioned: &[api::issue::Issue],
) -> serde_json::Value {
    serde_json::json!({
        "authored": authored,
        "review_requested": review_requested,
        "mentioned": mentioned,
    })
}

fn print_status_section(label: &str, items: &[api::issue::Issue]) {
    println!("{}", output::bold(label));
    if items.is_empty() {
        println!("  (none)");
        println!();
        return;
    }
    for i in items {
        println!(
            "  #{}  {}  {}",
            i.number,
            output::truncate(&i.title, 70),
            output::dim(&i.html_url)
        );
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_json_fields_can_project_authored() {
        let value = status_json_value(&[], &[], &[]);
        let projected = output::json_filter::project(&value, &["authored".into()]);
        assert_eq!(projected, serde_json::json!({ "authored": [] }));
    }
}
