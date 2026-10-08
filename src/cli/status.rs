//! `fj status`: notifications inbox.
//!
//! The bare command lists the inbox. The subcommands act on one thread
//! (`read`, `unsubscribe`) or manage the local repository mute list (`mute`,
//! `unmute`, `muted`).

use anyhow::{bail, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::api::notification::{Notification, Unsubscribed};
use crate::client::Client;
use crate::config::muted::{account_scope, MutedRepos};
use crate::output;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct StatusArgs {
    #[command(subcommand)]
    pub command: Option<StatusSub>,
    /// Show all (including already-read) notifications.
    #[arg(short = 'a', long)]
    pub all: bool,
    /// Maximum number of notifications to fetch
    #[arg(short = 'L', long, default_value_t = 20)]
    pub limit: u32,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
    /// Mark all notifications as read (muted repositories included).
    #[arg(long)]
    pub mark_read: bool,
    /// Also show threads from repositories muted with `fj status mute`.
    #[arg(long)]
    pub include_muted: bool,
}

#[derive(Debug, Subcommand)]
pub enum StatusSub {
    /// Mark one notification thread as read.
    Read(ThreadArgs),
    /// Unsubscribe from the issue or pull request behind a thread, and mark
    /// the thread read.
    Unsubscribe(ThreadArgs),
    /// Hide a repository's threads from `fj status` on this account.
    ///
    /// The mute is local to fj: it is stored in fj's config directory per
    /// account and never changes whether you watch the repository on the
    /// server. A muted repository still counts toward the server unread count.
    Mute(MuteArgs),
    /// Stop hiding a repository's threads.
    Unmute(MuteArgs),
    /// List the repositories muted on this account.
    Muted(MutedArgs),
}

#[derive(Debug, Args)]
pub struct ThreadArgs {
    /// Thread id, as shown in the ID column of `fj status`.
    pub id: u64,
}

#[derive(Debug, Args)]
pub struct MuteArgs {
    /// Repository as `<owner>/<name>`. Matched case-insensitively.
    pub repo: String,
}

#[derive(Debug, Args)]
pub struct MutedArgs {
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(args: StatusArgs, host: Option<&str>) -> Result<()> {
    match args.command {
        None => inbox(args, host).await,
        Some(StatusSub::Read(a)) => read(a, host).await,
        Some(StatusSub::Unsubscribe(a)) => unsubscribe(a, host).await,
        Some(StatusSub::Mute(a)) => mute(a, host),
        Some(StatusSub::Unmute(a)) => unmute(a, host),
        Some(StatusSub::Muted(a)) => muted(a, host),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn inbox(args: StatusArgs, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    if args.mark_read {
        api::notification::mark_all_read(&client).await?;
        println!("✓ Marked all notifications as read");
        return Ok(());
    }
    let (items, hidden, truncated) = if args.include_muted {
        let items = api::notification::list(&client, args.all, args.limit).await?;
        (items, 0, false)
    } else {
        let scope = account_scope(host)?;
        let mutes = MutedRepos::load()?;
        let inbox = api::notification::list_filtered(&client, args.all, args.limit, |n| {
            is_visible(&mutes, &scope, n)
        })
        .await?;
        (inbox.items, inbox.hidden, inbox.truncated)
    };
    // The note goes to stderr so it never lands in piped JSON or table output.
    if let Some(note) = hidden_note(hidden, truncated) {
        eprintln!("{}", output::dim(&note));
    }
    if args.json {
        return output::print_json(&serde_json::to_value(&items)?);
    }
    if items.is_empty() {
        println!("(no notifications)");
        return Ok(());
    }
    print!("{}", output::render_table(&HEADERS, &rows(&items)));
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn read(args: ThreadArgs, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    api::notification::mark_thread_read(&client, args.id).await?;
    // Re-read the thread: report what the server holds, not what was asked.
    let thread = api::notification::get_thread(&client, args.id).await?;
    if thread.unread {
        bail!(
            "the server accepted the request but thread {} is still unread",
            args.id
        );
    }
    println!(
        "✓ Marked thread {} as read ({})",
        thread.id,
        describe(&thread)
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn unsubscribe(args: ThreadArgs, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    let report = api::notification::unsubscribe_thread(&client, args.id).await?;
    let t = &report.target;
    let what = match report.outcome {
        Unsubscribed::Removed => "Unsubscribed",
        Unsubscribed::AlreadyUnsubscribed => "Already unsubscribed",
    };
    let state = if report.thread.unread {
        "still unread"
    } else {
        "read"
    };
    println!(
        "✓ {what} {} from {}/{}#{}; thread {} is {state}",
        report.user, t.owner, t.repo, t.index, report.thread.id
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn mute(args: MuteArgs, host: Option<&str>) -> Result<()> {
    api::split_repo(&args.repo)?;
    let scope = account_scope(host)?;
    let mut mutes = MutedRepos::load()?;
    if mutes.mute(&scope, &args.repo) {
        mutes.save()?;
        println!("✓ Muted {} for {scope}", args.repo);
    } else {
        println!("{} is already muted for {scope}", args.repo);
    }
    println!("{}", output::dim(MUTE_NOTE));
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn unmute(args: MuteArgs, host: Option<&str>) -> Result<()> {
    api::split_repo(&args.repo)?;
    let scope = account_scope(host)?;
    let mut mutes = MutedRepos::load()?;
    match mutes.unmute(&scope, &args.repo) {
        Some(stored) => {
            mutes.save()?;
            println!("✓ Unmuted {stored} for {scope}");
        }
        None => println!("{} is not muted for {scope}", args.repo),
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn muted(args: MutedArgs, host: Option<&str>) -> Result<()> {
    let scope = account_scope(host)?;
    let mutes = MutedRepos::load()?;
    let repos = mutes.list(&scope);
    if args.json {
        return output::print_json(&serde_json::to_value(repos)?);
    }
    if repos.is_empty() {
        println!("(no muted repositories for {scope})");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = repos.iter().map(|r| vec![r.clone()]).collect();
    print!("{}", output::render_table(&["REPO"], &rows));
    println!("{}", output::dim(MUTE_NOTE));
    Ok(())
}

const HEADERS: [&str; 7] = ["", "ID", "STATE", "TYPE", "REPO", "TITLE", "UPDATED"];

const MUTE_NOTE: &str = "Mutes are local to fj on this account. The server still delivers \
     these notifications and counts them as unread.";

/// Whether a thread survives the mute filter. A thread with no resolvable
/// repository always passes: there is nothing to match a mute against.
fn is_visible(mutes: &MutedRepos, scope: &str, thread: &Notification) -> bool {
    match thread.repository.as_ref() {
        Some(repo) if !repo.full_name.is_empty() => !mutes.is_muted(scope, &repo.full_name),
        _ => true,
    }
}

/// The stderr note for a filtered inbox, so an empty-looking inbox next to a
/// non-zero server unread count is explained instead of silently wrong.
fn hidden_note(hidden: usize, truncated: bool) -> Option<String> {
    if hidden == 0 && !truncated {
        return None;
    }
    let threads = if hidden == 1 { "thread" } else { "threads" };
    let mut note = format!(
        "{hidden} {threads} hidden from muted repositories (they still count toward the \
         server unread count; `fj status --include-muted` shows them, `fj status muted` \
         lists the mutes)"
    );
    if truncated {
        note.push_str("; stopped paging early, so older threads may be missing");
    }
    Some(note)
}

fn describe(n: &Notification) -> String {
    match n.repository.as_ref() {
        Some(r) if !r.full_name.is_empty() => format!("{}: {}", r.full_name, n.subject.title),
        _ => n.subject.title.clone(),
    }
}

fn rows(items: &[Notification]) -> Vec<Vec<String>> {
    items
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
                n.id.to_string(),
                output::state_pill(&n.subject.state, false),
                n.subject.type_.clone(),
                repo,
                output::truncate(&n.subject.title, 60),
                output::dim(&output::relative_time(n.updated_at)),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Harness {
        #[command(flatten)]
        status: StatusArgs,
    }

    fn parse(args: &[&str]) -> Result<StatusArgs, clap::Error> {
        let mut argv = vec!["status"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).map(|h| h.status)
    }

    fn thread(id: u64, repo: Option<&str>) -> Notification {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "unread": true,
            "pinned": false,
            "updated_at": "2026-01-01T00:00:00Z",
            "url": "https://forge.example/api/v1/notifications/threads/1",
            "subject": { "title": "a thing", "type": "Issue", "state": "open" },
            "repository": repo.map(|r| serde_json::json!({ "full_name": r }))
        }))
        .unwrap()
    }

    #[test]
    fn bare_form_and_existing_flags_still_parse() {
        let a = parse(&[]).unwrap();
        assert!(a.command.is_none());
        assert_eq!(a.limit, 20);
        assert!(!a.all && !a.json && !a.mark_read && !a.include_muted);

        let a = parse(&["--all", "--json", "--limit", "5"]).unwrap();
        assert!(a.all && a.json && a.command.is_none());
        assert_eq!(a.limit, 5);

        let a = parse(&["-a", "-L", "7"]).unwrap();
        assert!(a.all);
        assert_eq!(a.limit, 7);

        assert!(parse(&["--mark-read"]).unwrap().mark_read);
        assert!(parse(&["--include-muted"]).unwrap().include_muted);
    }

    #[test]
    fn subcommands_parse() {
        assert!(matches!(
            parse(&["read", "12"]).unwrap().command,
            Some(StatusSub::Read(ThreadArgs { id: 12 }))
        ));
        assert!(matches!(
            parse(&["unsubscribe", "3"]).unwrap().command,
            Some(StatusSub::Unsubscribe(ThreadArgs { id: 3 }))
        ));
        match parse(&["mute", "Acme/Widgets"]).unwrap().command {
            Some(StatusSub::Mute(m)) => assert_eq!(m.repo, "Acme/Widgets"),
            other => panic!("expected mute, got {other:?}"),
        }
        match parse(&["unmute", "acme/widgets"]).unwrap().command {
            Some(StatusSub::Unmute(m)) => assert_eq!(m.repo, "acme/widgets"),
            other => panic!("expected unmute, got {other:?}"),
        }
        assert!(matches!(
            parse(&["muted", "--json"]).unwrap().command,
            Some(StatusSub::Muted(MutedArgs { json: true }))
        ));
    }

    #[test]
    fn thread_commands_need_a_numeric_id() {
        assert!(parse(&["read"]).is_err());
        assert!(parse(&["read", "abc"]).is_err());
        assert!(parse(&["unsubscribe"]).is_err());
    }

    #[test]
    fn inbox_flags_do_not_mix_with_subcommands() {
        assert!(parse(&["--all", "read", "12"]).is_err());
        assert!(parse(&["--mark-read", "muted"]).is_err());
    }

    #[test]
    fn rows_carry_the_thread_id() {
        let rows = rows(&[thread(42, Some("o/r"))]);
        assert_eq!(HEADERS[1], "ID");
        assert_eq!(rows[0].len(), HEADERS.len());
        assert_eq!(rows[0][0], "•");
        assert_eq!(rows[0][1], "42");
        assert_eq!(rows[0][4], "o/r");
    }

    #[test]
    fn muted_repository_threads_are_filtered_case_insensitively() {
        let mut mutes = MutedRepos::default();
        mutes.mute("h", "Acme/Widgets");
        assert!(!is_visible(&mutes, "h", &thread(1, Some("acme/widgets"))));
        assert!(is_visible(&mutes, "h", &thread(2, Some("acme/other"))));
        // Another account's mutes do not apply.
        assert!(is_visible(
            &mutes,
            "other",
            &thread(3, Some("acme/widgets"))
        ));
    }

    #[test]
    fn threads_without_a_repository_pass_the_mute_filter() {
        let mut mutes = MutedRepos::default();
        mutes.mute("h", "o/r");
        assert!(is_visible(&mutes, "h", &thread(1, None)));
        assert!(is_visible(&mutes, "h", &thread(2, Some(""))));
    }

    #[test]
    fn hidden_note_explains_the_server_unread_count() {
        assert_eq!(hidden_note(0, false), None);
        let one = hidden_note(1, false).unwrap();
        assert!(one.starts_with("1 thread hidden"), "{one}");
        assert!(one.contains("server unread count"), "{one}");
        assert!(!one.contains("stopped paging"), "{one}");
        let many = hidden_note(1000, true).unwrap();
        assert!(many.starts_with("1000 threads hidden"), "{many}");
        assert!(many.contains("stopped paging early"), "{many}");
    }

    #[test]
    fn describe_names_the_repository_when_known() {
        assert_eq!(describe(&thread(1, Some("o/r"))), "o/r: a thing");
        assert_eq!(describe(&thread(1, None)), "a thing");
    }
}
