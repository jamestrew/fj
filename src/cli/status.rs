//! `fj status` — notifications inbox.

use anyhow::Result;
use clap::Args;

use crate::api;
use crate::client::Client;
use crate::output;

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Show all (including already-read) notifications.
    #[arg(short = 'a', long)]
    pub all: bool,
    /// Maximum number of notifications to fetch
    #[arg(short = 'L', long, default_value_t = 20)]
    pub limit: u32,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
    /// Mark all notifications as read.
    #[arg(long)]
    pub mark_read: bool,
}

pub async fn run(args: StatusArgs, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    if args.mark_read {
        api::notification::mark_all_read(&client).await?;
        println!("✓ Marked all notifications as read");
        return Ok(());
    }
    let items = api::notification::list(&client, args.all, args.limit).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&items)?);
    }
    if items.is_empty() {
        println!("(no notifications)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|n| {
            let mark = if n.unread { "•" } else { " " };
            let repo = n
                .repository
                .as_ref()
                .map(|r| r.full_name.clone())
                .unwrap_or_default();
            vec![
                mark.into(),
                output::state_pill(&n.subject.state, false),
                n.subject.type_.clone(),
                repo,
                output::truncate(&n.subject.title, 60),
                output::dim(&output::relative_time(n.updated_at)),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["", "STATE", "TYPE", "REPO", "TITLE", "UPDATED"], &rows)
    );
    Ok(())
}
