//! `fj issue time`: tracked time on an issue or pull request (they share the
//! issue index, so the number may be a pull request's). The total printed is
//! always summed from the entries the forge returned, never kept locally.

use anyhow::{anyhow, bail, Result};
use clap::{Args, Subcommand};
use reqwest::StatusCode;

use crate::api;
use crate::api::issue_time::TrackedTime;
use crate::api::issue_tracker::{status_of, work_item_exists, RepoTracker};
use crate::cli::context::{resolve_repo, RepoContext, RepoFlag};
use crate::client::ApiError;
use crate::output;

use super::editor;

#[derive(Debug, Args)]
pub struct TimeCmd {
    #[command(subcommand)]
    pub command: TimeSub,
}

#[derive(Debug, Subcommand)]
pub enum TimeSub {
    /// List tracked time entries with their author, duration, and the total.
    List(ListArgs),
    /// Log time as yourself, e.g. `fj issue time add 12 1h30m`.
    Add(AddArgs),
    /// Delete a time entry by id (ids come from `issue time list`).
    Delete(DeleteArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Issue or pull request number
    pub number: u64,
    /// Maximum number of entries to show (the total always covers all of them)
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Issue or pull request number
    pub number: u64,
    /// Duration: `1h30m`, `45m`, `2h`, `90s`, or `1:30` (hours:minutes)
    pub duration: String,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Issue or pull request number
    pub number: u64,
    /// Time entry id
    pub id: u64,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

/// Seconds in a typed duration. Accepts unit runs (`1h30m`, `45m`, `90s`,
/// spaces allowed) and `H:MM`. A bare number is refused as ambiguous, and so
/// is a zero duration.
fn parse_duration(input: &str) -> Result<u64> {
    let invalid = || anyhow!("'{input}' is not a duration (use 1h30m, 45m, 90s, or 1:30)");
    let text: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    let seconds = if let Some((h, m)) = text.split_once(':') {
        let h: u64 = h.parse().map_err(|_| invalid())?;
        let m: u64 = m.parse().map_err(|_| invalid())?;
        if m >= 60 {
            return Err(invalid());
        }
        h.checked_mul(3600).and_then(|s| s.checked_add(m * 60))
    } else {
        let mut total: Option<u64> = Some(0);
        let mut digits = String::new();
        let mut units = 0;
        for c in text.chars() {
            if c.is_ascii_digit() {
                digits.push(c);
                continue;
            }
            let scale = match c.to_ascii_lowercase() {
                'h' => 3600,
                'm' => 60,
                's' => 1,
                _ => return Err(invalid()),
            };
            let n: u64 = digits.parse().map_err(|_| invalid())?;
            digits.clear();
            units += 1;
            total = total.and_then(|t| n.checked_mul(scale).and_then(|s| t.checked_add(s)));
        }
        if !digits.is_empty() || units == 0 {
            return Err(invalid());
        }
        total
    };
    match seconds {
        Some(0) => Err(anyhow!("a time entry must be longer than zero")),
        Some(s) => Ok(s),
        None => Err(invalid()),
    }
}

/// `3900` as `1h 5m`. Seconds show only when there are any.
fn format_duration(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(format!("{h}h"));
    }
    if m > 0 || (h > 0 && s > 0) {
        parts.push(format!("{m}m"));
    }
    if s > 0 {
        parts.push(format!("{s}s"));
    }
    if parts.is_empty() {
        return "0m".to_string();
    }
    parts.join(" ")
}

fn total_seconds(entries: &[TrackedTime]) -> u64 {
    entries.iter().map(|e| e.time).sum()
}

fn total_line(entries: &[TrackedTime]) -> String {
    let n = entries.len();
    format!(
        "Total: {} across {n} {}",
        format_duration(total_seconds(entries)),
        if n == 1 { "entry" } else { "entries" }
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Read,
    Add,
    Delete(u64),
}

/// Why the forge would not do it, as far as we can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Refusal {
    Disabled,
    Denied,
    MissingItem,
    MissingEntry(u64),
}

impl Refusal {
    fn message(&self, repo: &str, number: u64, op: Op, contributors_only: bool) -> String {
        match self {
            Refusal::Disabled => format!("time tracking is disabled on {repo}"),
            Refusal::Denied => {
                let what = match op {
                    Op::Read => format!("may not read tracked time on {repo} #{number}"),
                    Op::Add => format!("may not track time on {repo} #{number}"),
                    Op::Delete(id) => format!(
                        "may not delete time entry {id} on {repo} #{number} (only its author or an administrator can)"
                    ),
                };
                let why = if contributors_only && op == Op::Add {
                    " (only contributors may track time in this repository)"
                } else {
                    ""
                };
                format!("permission denied: your account {what}{why}")
            }
            Refusal::MissingItem => format!(
                "work item #{number} not found in {repo} (it does not exist, or your account cannot read it)"
            ),
            Refusal::MissingEntry(id) => {
                format!("time entry {id} not found on {repo} #{number}")
            }
        }
    }
}

/// Read a failure status. Forgejo answers a read with 404 both for a missing
/// work item and for time tracking being off, and a write with 400 when it is
/// off, so `this_exists` (probed by the caller on a 404) and the repository's
/// own switch decide between them.
fn classify(
    op: Op,
    status: StatusCode,
    server_message: &str,
    enabled: Option<bool>,
    this_exists: bool,
) -> Option<Refusal> {
    match status {
        StatusCode::FORBIDDEN => Some(Refusal::Denied),
        StatusCode::BAD_REQUEST => {
            let says_off = server_message.to_ascii_lowercase().contains("disabled");
            (says_off || enabled != Some(true)).then_some(Refusal::Disabled)
        }
        StatusCode::NOT_FOUND if !this_exists => Some(Refusal::MissingItem),
        StatusCode::NOT_FOUND => match op {
            Op::Read => Some(Refusal::Disabled),
            Op::Add => (enabled != Some(true)).then_some(Refusal::Disabled),
            Op::Delete(id) => Some(Refusal::MissingEntry(id)),
        },
        _ => None,
    }
}

fn preflight(tracker: &RepoTracker) -> Option<Refusal> {
    (tracker.time_tracking_enabled() == Some(false)).then_some(Refusal::Disabled)
}

fn entries_table(entries: &[TrackedTime]) -> String {
    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|e| {
            vec![
                e.id.to_string(),
                e.user_name.clone(),
                format_duration(e.time),
                output::dim(&e.created.map(output::relative_time).unwrap_or_default()),
            ]
        })
        .collect();
    output::render_table(&["ID", "AUTHOR", "DURATION", "LOGGED"], &rows)
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: TimeCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        TimeSub::List(args) => list(args, host).await,
        TimeSub::Add(args) => add(args, host).await,
        TimeSub::Delete(args) => delete(args, host).await,
    }
}

/// Turn a forge failure into the distinct message it stands for, keeping the
/// original error as the cause. Anything we cannot read is passed through.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn explain(
    ctx: &RepoContext,
    tracker: &RepoTracker,
    number: u64,
    op: Op,
    err: anyhow::Error,
) -> anyhow::Error {
    let Some(status) = status_of(&err) else {
        return err;
    };
    let mut this_exists = true;
    if status == StatusCode::NOT_FOUND {
        match work_item_exists(&ctx.client, &ctx.owner, &ctx.name, number).await {
            Ok(found) => this_exists = found,
            Err(_) => return err,
        }
    }
    let server_message = err
        .downcast_ref::<ApiError>()
        .map(|e| e.message.clone())
        .unwrap_or_default();
    let refusal = classify(
        op,
        status,
        &server_message,
        tracker.time_tracking_enabled(),
        this_exists,
    );
    match refusal {
        Some(r) => err.context(r.message(
            &ctx.slug(),
            number,
            op,
            tracker.time_limited_to_contributors(),
        )),
        None => err,
    }
}

/// The repository's tracker facts, refusing up front when it says time
/// tracking is off. A forge that does not say is asked anyway.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn tracker(ctx: &RepoContext, number: u64, op: Op) -> Result<RepoTracker> {
    let tracker = api::issue_tracker::get(&ctx.client, &ctx.owner, &ctx.name).await?;
    if let Some(refusal) = preflight(&tracker) {
        bail!(refusal.message(&ctx.slug(), number, op, false));
    }
    Ok(tracker)
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let tracker = tracker(&ctx, args.number, Op::Read).await?;
    let entries = match api::issue_time::list(&ctx.client, &ctx.owner, &ctx.name, args.number).await
    {
        Ok(entries) => entries,
        Err(err) => return Err(explain(&ctx, &tracker, args.number, Op::Read, err).await),
    };
    let shown = &entries[..entries.len().min(args.limit.max(1) as usize)];
    if args.json {
        return output::print_json(&serde_json::json!({
            "total_count": entries.len(),
            "total_seconds": total_seconds(&entries),
            "items": shown,
        }));
    }
    if entries.is_empty() {
        println!("(no tracked time)");
        return Ok(());
    }
    println!("{}", entries_table(shown));
    if shown.len() < entries.len() {
        println!(
            "{}",
            output::dim(&format!(
                "Showing {} of {}. Use --limit for more.",
                shown.len(),
                entries.len()
            ))
        );
    }
    println!("{}", total_line(&entries));
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn add(args: AddArgs, host: Option<&str>) -> Result<()> {
    let seconds = parse_duration(&args.duration)?;
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let tracker = tracker(&ctx, args.number, Op::Add).await?;
    let entry = match api::issue_time::add(&ctx.client, &ctx.owner, &ctx.name, args.number, seconds)
        .await
    {
        Ok(entry) => entry,
        Err(err) => return Err(explain(&ctx, &tracker, args.number, Op::Add, err).await),
    };
    println!(
        "✓ Logged {} on #{} as {} (entry {})",
        format_duration(entry.time),
        args.number,
        entry.user_name,
        entry.id
    );
    let entries = api::issue_time::list(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    println!("{}", total_line(&entries));
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn delete(args: DeleteArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let op = Op::Delete(args.id);
    let tracker = tracker(&ctx, args.number, op).await?;
    if !args.yes {
        let ans = editor::prompt_line(&format!(
            "Delete time entry {} on #{}? Type the id to confirm",
            args.id, args.number
        ))?;
        if ans != args.id.to_string() {
            return Err(anyhow!("aborted"));
        }
    }
    if let Err(err) =
        api::issue_time::delete(&ctx.client, &ctx.owner, &ctx.name, args.number, args.id).await
    {
        return Err(explain(&ctx, &tracker, args.number, op, err).await);
    }
    println!("✓ Deleted time entry {} from #{}", args.id, args.number);
    let entries = api::issue_time::list(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    println!("{}", total_line(&entries));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::issue::{IssueCmd, IssueSub};
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn entry(id: u64, user: &str, time: u64) -> TrackedTime {
        serde_json::from_value(serde_json::json!({
            "id": id, "user_name": user, "time": time,
            "created": "2026-01-01T00:00:00Z",
            "issue": { "number": 3 }
        }))
        .unwrap()
    }

    fn parse(argv: &[&str]) -> TimeSub {
        match Cli::try_parse_from(argv).expect("should parse").command {
            Command::Issue(IssueCmd {
                command: IssueSub::Time(cmd),
            }) => cmd.command,
            other => panic!("expected issue time, got {other:?}"),
        }
    }

    #[test]
    fn subcommands_parse() {
        match parse(&["fj", "issue", "time", "list", "3", "--json", "-L", "5"]) {
            TimeSub::List(a) => assert!(a.number == 3 && a.json && a.limit == 5),
            other => panic!("expected list, got {other:?}"),
        }
        match parse(&["fj", "issue", "time", "add", "3", "1h30m"]) {
            TimeSub::Add(a) => assert_eq!(a.duration, "1h30m"),
            other => panic!("expected add, got {other:?}"),
        }
        match parse(&["fj", "issue", "time", "delete", "3", "17", "-y"]) {
            TimeSub::Delete(a) => assert!(a.id == 17 && a.yes),
            other => panic!("expected delete, got {other:?}"),
        }
    }

    #[test]
    fn parses_durations_to_seconds() {
        assert_eq!(parse_duration("1h30m").unwrap(), 5400);
        assert_eq!(parse_duration("1H 30M").unwrap(), 5400);
        assert_eq!(parse_duration("45m").unwrap(), 2700);
        assert_eq!(parse_duration("90m").unwrap(), 5400);
        assert_eq!(parse_duration("2h").unwrap(), 7200);
        assert_eq!(parse_duration("90s").unwrap(), 90);
        assert_eq!(parse_duration("1:30").unwrap(), 5400);
        assert_eq!(parse_duration("0:05").unwrap(), 300);
    }

    #[test]
    fn refuses_empty_zero_and_malformed_durations() {
        for bad in ["", "90", "h", "1x", "1h30", "1:60", "1:", "-1h", "1.5h"] {
            let err = parse_duration(bad).unwrap_err().to_string();
            assert!(err.contains("is not a duration"), "{bad}: {err}");
        }
        for zero in ["0m", "0h0m", "0:00"] {
            let err = parse_duration(zero).unwrap_err().to_string();
            assert!(err.contains("longer than zero"), "{zero}: {err}");
        }
        assert!(parse_duration("99999999999999999999h").is_err());
        assert!(parse_duration("18446744073709551615h").is_err());
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(0), "0m");
        assert_eq!(format_duration(90), "1m 30s");
        assert_eq!(format_duration(2700), "45m");
        assert_eq!(format_duration(3600), "1h");
        assert_eq!(format_duration(5400), "1h 30m");
        assert_eq!(format_duration(3605), "1h 0m 5s");
    }

    #[test]
    fn total_is_summed_from_the_entries() {
        let entries = [entry(1, "alice", 3600), entry(2, "bob", 1800)];
        assert_eq!(total_seconds(&entries), 5400);
        assert_eq!(total_line(&entries), "Total: 1h 30m across 2 entries");
        assert_eq!(total_line(&entries[..1]), "Total: 1h across 1 entry");
        assert_eq!(total_line(&[]), "Total: 0m across 0 entries");
        let table = entries_table(&entries);
        assert!(table.contains("alice") && table.contains("30m") && table.contains("AUTHOR"));
    }

    #[test]
    fn preflight_refuses_only_when_the_repository_says_off() {
        let repo = |json: &str| serde_json::from_str::<RepoTracker>(json).unwrap();
        assert_eq!(
            preflight(&repo(
                r#"{"internal_tracker":{"enable_time_tracker":false}}"#
            )),
            Some(Refusal::Disabled)
        );
        assert_eq!(
            preflight(&repo(
                r#"{"internal_tracker":{"enable_time_tracker":true}}"#
            )),
            None
        );
        assert_eq!(preflight(&repo("{}")), None);
    }

    #[test]
    fn classify_tells_the_failures_apart() {
        use StatusCode as S;
        assert_eq!(
            classify(Op::Add, S::FORBIDDEN, "", Some(true), true),
            Some(Refusal::Denied)
        );
        // The forge answers a write with 400 when time tracking is off.
        assert_eq!(
            classify(
                Op::Add,
                S::BAD_REQUEST,
                "time tracking disabled",
                Some(true),
                true
            ),
            Some(Refusal::Disabled)
        );
        assert_eq!(
            classify(Op::Add, S::BAD_REQUEST, "", None, true),
            Some(Refusal::Disabled)
        );
        assert_eq!(
            classify(Op::Add, S::BAD_REQUEST, "bad body", Some(true), true),
            None
        );
        assert_eq!(
            classify(Op::Read, S::NOT_FOUND, "", Some(true), false),
            Some(Refusal::MissingItem)
        );
        // A read of an existing work item that 404s means the feature is off.
        assert_eq!(
            classify(Op::Read, S::NOT_FOUND, "", Some(true), true),
            Some(Refusal::Disabled)
        );
        assert_eq!(
            classify(Op::Delete(9), S::NOT_FOUND, "", Some(true), true),
            Some(Refusal::MissingEntry(9))
        );
        assert_eq!(classify(Op::Add, S::NOT_FOUND, "", Some(true), true), None);
        assert_eq!(
            classify(Op::Add, S::INTERNAL_SERVER_ERROR, "", None, true),
            None
        );
    }

    #[test]
    fn each_refusal_has_its_own_message() {
        let off = Refusal::Disabled.message("o/r", 3, Op::Add, false);
        assert_eq!(off, "time tracking is disabled on o/r");

        let denied = Refusal::Denied.message("o/r", 3, Op::Add, false);
        assert_eq!(
            denied,
            "permission denied: your account may not track time on o/r #3"
        );
        let contributors = Refusal::Denied.message("o/r", 3, Op::Add, true);
        assert!(contributors.contains("only contributors may track time"));
        let not_yours = Refusal::Denied.message("o/r", 3, Op::Delete(9), true);
        assert!(not_yours.contains("may not delete time entry 9"));
        assert!(!not_yours.contains("only contributors"));
        assert!(Refusal::Denied
            .message("o/r", 3, Op::Read, false)
            .contains("may not read"));

        assert!(Refusal::MissingItem
            .message("o/r", 3, Op::Read, false)
            .starts_with("work item #3 not found in o/r"));
        assert_eq!(
            Refusal::MissingEntry(9).message("o/r", 3, Op::Delete(9), false),
            "time entry 9 not found on o/r #3"
        );
    }
}
