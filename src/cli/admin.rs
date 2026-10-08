//! `fj admin`: instance administration for the connected forge. Every
//! command needs an instance-administrator token.

use std::io::IsTerminal;

use anyhow::{anyhow, bail, Context, Result};
use clap::{ArgGroup, Args, Subcommand};
use reqwest::StatusCode;

use crate::api::admin::{self, CreateUser, Cron, Section, UnadoptedQuery};
use crate::client::{ApiError, Client};
use crate::output;

#[derive(Debug, Args)]
pub struct AdminCmd {
    #[command(subcommand)]
    pub command: AdminSub,
}

#[derive(Debug, Subcommand)]
pub enum AdminSub {
    /// Show the forge-wide settings the server exposes.
    Settings(SettingsArgs),
    /// Inspect scheduled (cron) tasks.
    Cron(CronCmd),
    /// Manage forge users.
    User(UserCmd),
    /// Manage unadopted repositories (files on disk with no repository record).
    Unadopted(UnadoptedCmd),
}

#[derive(Debug, Args)]
pub struct SettingsArgs {
    /// Emit JSON instead of text
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CronCmd {
    #[command(subcommand)]
    pub command: CronSub,
}

#[derive(Debug, Subcommand)]
pub enum CronSub {
    /// List scheduled tasks and their status.
    List(CronListArgs),
}

#[derive(Debug, Args)]
pub struct CronListArgs {
    /// Maximum number of tasks to list
    #[arg(short = 'L', long, default_value_t = 100)]
    pub limit: usize,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct UserCmd {
    #[command(subcommand)]
    pub command: UserSub,
}

#[derive(Debug, Subcommand)]
pub enum UserSub {
    /// Create a user. The initial password is never taken from a flag.
    Create(UserCreateArgs),
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("credential")
        .required(true)
        .args(["password_stdin", "password_prompt", "no_password"])
))]
pub struct UserCreateArgs {
    /// Username for the new account
    pub username: String,
    /// Email address for the new account
    #[arg(long)]
    pub email: String,
    /// Read the initial password from stdin (for example from a password manager)
    #[arg(long)]
    pub password_stdin: bool,
    /// Prompt for the initial password without echo (needs a terminal)
    #[arg(long)]
    pub password_prompt: bool,
    /// Submit no password. For accounts on an external login source, or when
    /// the forge is expected to handle the credential itself
    #[arg(long)]
    pub no_password: bool,
    /// Whether the user must change the password at first sign-in
    #[arg(
        long,
        value_name = "BOOL",
        action = clap::ArgAction::Set,
        required_unless_present = "no_password",
        conflicts_with = "no_password"
    )]
    pub must_change_password: Option<bool>,
    /// Whether the forge emails the new user a registration notice
    #[arg(long, value_name = "BOOL", action = clap::ArgAction::Set)]
    pub send_notify: bool,
    /// Display name
    #[arg(long)]
    pub full_name: Option<String>,
    /// Login name on the external login source
    #[arg(long)]
    pub login_name: Option<String>,
    /// Id of the login source that authenticates this user
    #[arg(long)]
    pub source_id: Option<i64>,
    /// Profile visibility
    #[arg(long, value_parser = ["public", "limited", "private"])]
    pub visibility: Option<String>,
    /// Create the account as restricted
    #[arg(long)]
    pub restricted: bool,
    /// Emit the server's account record as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct UnadoptedCmd {
    #[command(subcommand)]
    pub command: UnadoptedSub,
}

#[derive(Debug, Subcommand)]
pub enum UnadoptedSub {
    /// List unadopted repositories.
    List(UnadoptedListArgs),
    /// Adopt on-disk files as a repository owned by <owner>.
    Adopt(UnadoptedActArgs),
    /// Permanently delete an unadopted repository's files from the server.
    Delete(UnadoptedActArgs),
}

#[derive(Debug, Args)]
pub struct UnadoptedListArgs {
    /// Glob pattern to match against `owner/name`
    #[arg(long)]
    pub pattern: Option<String>,
    /// Page of results to fetch
    #[arg(long, default_value_t = 1)]
    pub page: u32,
    /// Results per page
    #[arg(short = 'L', long, default_value_t = 50)]
    pub limit: u32,
    /// Emit JSON instead of text
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct UnadoptedActArgs {
    /// The unadopted repository as `owner/name`
    pub repo: String,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: AdminCmd, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    let result = match cmd.command {
        AdminSub::Settings(args) => settings(&client, args).await,
        AdminSub::Cron(c) => match c.command {
            CronSub::List(args) => cron_list(&client, args).await,
        },
        AdminSub::User(c) => match c.command {
            UserSub::Create(args) => user_create(&client, args).await,
        },
        AdminSub::Unadopted(c) => match c.command {
            UnadoptedSub::List(args) => unadopted_list(&client, args).await,
            UnadoptedSub::Adopt(args) => unadopted_act(&client, args, Act::Adopt).await,
            UnadoptedSub::Delete(args) => unadopted_act(&client, args, Act::Delete).await,
        },
    };
    result.map_err(|err| admin_context(err, client.host()))
}

/// Give a 403 its own headline: the caller is not an instance administrator
/// (or the token lacks the admin scope). Every other failure passes through
/// unchanged, so a denial never reads like a missing endpoint or a bad
/// request. The `ApiError` stays in the chain for the exit code.
fn admin_context(err: anyhow::Error, host: &str) -> anyhow::Error {
    let denial = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<ApiError>())
        .filter(|api| api.status == StatusCode::FORBIDDEN)
        .map(|api| api.message.trim().to_string());
    match denial {
        Some(message) => err.context(admin_required(host, &message)),
        None => err,
    }
}

fn admin_required(host: &str, server_message: &str) -> String {
    if server_message.is_empty() {
        format!("requires an instance administrator on {host} (HTTP 403)")
    } else {
        format!("requires an instance administrator on {host} (HTTP 403: {server_message})")
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn settings(client: &Client, args: SettingsArgs) -> Result<()> {
    // The settings endpoints themselves are readable by any signed-in user,
    // so the administrator check is made here against the server's own view
    // of the account.
    let me = crate::api::user::current(client).await?;
    if !me.is_admin {
        bail!(
            "requires an instance administrator on {}: the forge reports @{} is not one",
            client.host(),
            me.login
        );
    }
    let mut sections: Vec<(&str, Section)> = Vec::new();
    for (name, path) in admin::SETTINGS_SECTIONS {
        sections.push((name, admin::settings_section(client, path).await?));
    }
    if args.json {
        return output::print_json(&settings_json(&sections));
    }
    print!("{}", render_settings(&sections));
    Ok(())
}

fn settings_json(sections: &[(&str, Section)]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    let mut unavailable = Vec::new();
    for (name, section) in sections {
        match section {
            Section::Available(value) => {
                map.insert((*name).to_string(), value.clone());
            }
            Section::Unavailable(_) => {
                map.insert((*name).to_string(), serde_json::Value::Null);
                unavailable.push(serde_json::Value::String((*name).to_string()));
            }
        }
    }
    map.insert("unavailable".into(), serde_json::Value::Array(unavailable));
    serde_json::Value::Object(map)
}

/// Text view of the settings sections. Only keys the server sent are shown;
/// a section the forge does not serve is labeled unavailable.
fn render_settings(sections: &[(&str, Section)]) -> String {
    let mut out = String::new();
    for (name, section) in sections {
        match section {
            Section::Unavailable(status) => {
                out.push_str(&format!(
                    "{name}: unavailable on this forge (HTTP {status})\n"
                ));
            }
            Section::Available(serde_json::Value::Object(map)) => {
                out.push_str(&format!("{name}:\n"));
                if map.is_empty() {
                    out.push_str("  (the server sent no values)\n");
                }
                let width = map.keys().map(String::len).max().unwrap_or(0);
                for (key, value) in map {
                    out.push_str(&format!("  {key:<width$}  {}\n", setting_value(value)));
                }
            }
            Section::Available(other) => {
                out.push_str(&format!("{name}: {}\n", setting_value(other)));
            }
        }
    }
    out
}

fn setting_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) if items.is_empty() => "(none)".to_string(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(setting_value)
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn cron_list(client: &Client, args: CronListArgs) -> Result<()> {
    let Some(tasks) = admin::cron_list(client, args.limit).await? else {
        bail!(
            "scheduled tasks are unavailable on {}: the forge does not serve /admin/cron",
            client.host()
        );
    };
    if args.json {
        return output::print_json(&serde_json::to_value(&tasks)?);
    }
    if tasks.is_empty() {
        println!("(the server reported no scheduled tasks)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = tasks.iter().map(cron_row).collect();
    print!(
        "{}",
        output::render_table(&["NAME", "SCHEDULE", "NEXT", "PREVIOUS", "RUNS"], &rows)
    );
    Ok(())
}

fn cron_row(task: &Cron) -> Vec<String> {
    vec![
        task.name.clone(),
        task.schedule.clone().unwrap_or_else(|| "unknown".into()),
        cron_time(task.next),
        cron_time(task.prev),
        task.exec_times
            .map_or_else(|| "unknown".into(), |n| n.to_string()),
    ]
}

/// A task time as the server sent it. Forgejo reports Go's zero time (year 1)
/// for a task that has not run or is not scheduled.
fn cron_time(time: Option<chrono::DateTime<chrono::Utc>>) -> String {
    use chrono::Datelike;
    match time {
        None => "unknown".to_string(),
        Some(t) if t.year() <= 1 => "never".to_string(),
        Some(t) => t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    }
}

/// How the initial credential reaches fj. Never a flag value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialSource {
    Stdin,
    Prompt,
    None,
}

fn credential_source(args: &UserCreateArgs) -> CredentialSource {
    if args.password_stdin {
        CredentialSource::Stdin
    } else if args.password_prompt {
        CredentialSource::Prompt
    } else {
        CredentialSource::None
    }
}

/// Refuse the two combinations that would either echo the password or hang:
/// `--password-stdin` on a terminal (typed input is echoed) and
/// `--password-prompt` without one (nothing can answer the prompt).
fn check_credential_source(source: CredentialSource, stdin_is_tty: bool) -> Result<()> {
    match (source, stdin_is_tty) {
        (CredentialSource::Stdin, true) => Err(anyhow!(
            "--password-stdin reads a piped password, but stdin is a terminal and would echo it; \
             pipe the password in or use --password-prompt"
        )),
        (CredentialSource::Prompt, false) => Err(anyhow!(
            "--password-prompt needs a terminal; pipe the password with --password-stdin instead"
        )),
        _ => Ok(()),
    }
}

/// Take the password from piped stdin text: one trailing line ending is
/// dropped, nothing else is trimmed (leading or trailing spaces are legal in
/// a password).
fn password_from_stdin(raw: &str) -> Result<String> {
    let pw = raw
        .strip_suffix("\r\n")
        .or_else(|| raw.strip_suffix('\n'))
        .unwrap_or(raw);
    if pw.is_empty() {
        bail!("no password on stdin");
    }
    if pw.contains('\n') {
        bail!("stdin held more than one line; pipe exactly the password");
    }
    Ok(pw.to_string())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn read_password(source: CredentialSource) -> Result<Option<String>> {
    check_credential_source(source, std::io::stdin().is_terminal())?;
    match source {
        CredentialSource::None => Ok(None),
        CredentialSource::Stdin => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("reading the password from stdin")?;
            password_from_stdin(&buf).map(Some)
        }
        CredentialSource::Prompt => dialoguer::Password::new()
            .with_prompt("Initial password")
            .with_confirmation("Repeat password", "passwords do not match")
            .interact()
            .map(Some)
            .context("reading the password"),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn user_create(client: &Client, args: UserCreateArgs) -> Result<()> {
    let source = credential_source(&args);
    // The password lives only in this binding, for the one request.
    let password = read_password(source)?;
    let body = CreateUser {
        username: &args.username,
        email: &args.email,
        password: password.as_deref(),
        must_change_password: args.must_change_password,
        send_notify: args.send_notify,
        full_name: args.full_name.as_deref(),
        login_name: args.login_name.as_deref(),
        source_id: args.source_id,
        visibility: args.visibility.as_deref(),
        restricted: args.restricted.then_some(true),
    };
    let created = admin::create_user(client, &body).await?;
    drop(password);
    if args.json {
        return output::print_json(&created);
    }
    print!(
        "{}",
        created_summary(
            &created,
            source,
            args.must_change_password,
            args.send_notify
        )
    );
    Ok(())
}

/// Summary of the account the server confirmed. Account fields come from the
/// response only. The response does not report the password-change or
/// notification outcome, so those lines say what was requested and that the
/// server did not confirm it.
fn created_summary(
    created: &serde_json::Value,
    source: CredentialSource,
    must_change_password: Option<bool>,
    send_notify: bool,
) -> String {
    let text = |key: &str| created.get(key).and_then(|v| v.as_str());
    let mut out = String::new();
    let login = text("login").unwrap_or("(login not reported)");
    match created.get("id").and_then(|v| v.as_i64()) {
        Some(id) => out.push_str(&format!("✓ Created user {login} (id {id})\n")),
        None => out.push_str(&format!("✓ Created user {login}\n")),
    }
    for (label, key) in [
        ("email", "email"),
        ("full name", "full_name"),
        ("visibility", "visibility"),
    ] {
        if let Some(value) = text(key).filter(|v| !v.is_empty()) {
            out.push_str(&format!("  {label}: {value}\n"));
        }
    }
    for (label, key) in [("administrator", "is_admin"), ("restricted", "restricted")] {
        if let Some(flag) = created.get(key).and_then(|v| v.as_bool()) {
            out.push_str(&format!("  {label}: {}\n", if flag { "yes" } else { "no" }));
        }
    }
    out.push_str(match source {
        CredentialSource::Stdin => "  initial credential: password read from stdin (not shown)\n",
        CredentialSource::Prompt => {
            "  initial credential: password entered at the prompt (not shown)\n"
        }
        CredentialSource::None => {
            "  initial credential: none submitted; the forge decides how this account signs in\n"
        }
    });
    if let Some(must) = must_change_password {
        out.push_str(&format!(
            "  password change at first sign-in: {} (not confirmed in the server response)\n",
            if must {
                "requested"
            } else {
                "not required, as requested"
            }
        ));
    }
    out.push_str(&format!(
        "  notification email: {} (not confirmed in the server response)\n",
        if send_notify {
            "requested"
        } else {
            "not requested"
        }
    ));
    out
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn unadopted_list(client: &Client, args: UnadoptedListArgs) -> Result<()> {
    let repos = admin::unadopted_list(
        client,
        &UnadoptedQuery {
            pattern: args.pattern.as_deref(),
            page: Some(args.page),
            limit: Some(args.limit),
        },
    )
    .await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&repos)?);
    }
    if repos.is_empty() {
        println!("(no unadopted repositories on this page)");
        return Ok(());
    }
    for repo in &repos {
        println!("{repo}");
    }
    if repos.len() as u64 >= u64::from(args.limit) {
        eprintln!(
            "{}",
            output::dim(&format!(
                "page {} is full; there may be more (--page {})",
                args.page,
                args.page + 1
            ))
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Adopt,
    Delete,
}

/// What the administrator sees before deciding: where the files are and what
/// the action does to them.
fn unadopted_preamble(act: Act, owner: &str, repo: &str) -> String {
    let consequence = match act {
        Act::Adopt => format!(
            "adopting makes these files a repository owned by {owner}, visible and usable as {owner}/{repo}"
        ),
        Act::Delete => {
            "deleting permanently removes these files from the server; this cannot be undone"
                .to_string()
        }
    };
    format!(
        "Unadopted repository: {owner}/{repo}\n\
         Source path: {owner}/{repo}.git under the forge's repository root \
         (the API does not expose the absolute path)\n\
         Consequence: {consequence}\n"
    )
}

/// Decide whether to prompt. `Ok(false)` skips it (`--yes`), `Ok(true)`
/// prompts, and a non-interactive run without `--yes` is refused instead of
/// waiting on input that cannot arrive.
pub(super) fn confirm_gate(yes: bool, stdin_is_tty: bool) -> Result<bool> {
    match (yes, stdin_is_tty) {
        (true, _) => Ok(false),
        (false, true) => Ok(true),
        (false, false) => Err(anyhow!(
            "refusing to continue without confirmation: stdin is not a terminal, pass --yes"
        )),
    }
}

/// Typed confirmation: the user must type `expected` back.
#[cfg_attr(coverage_nightly, coverage(off))]
pub(super) fn confirm_typed(yes: bool, question: &str, expected: &str) -> Result<()> {
    if !confirm_gate(yes, std::io::stdin().is_terminal())? {
        return Ok(());
    }
    let answer =
        crate::cli::editor::prompt_line(&format!("{question} Type '{expected}' to confirm"))?;
    if answer != expected {
        bail!("aborted");
    }
    Ok(())
}

/// Wording for a mutation the server did not confirm, so a failure can never
/// read as if the action happened.
fn not_confirmed(act: Act, slug: &str, err: &anyhow::Error) -> String {
    let (what, state) = match act {
        Act::Adopt => ("adoption", "it was not adopted"),
        Act::Delete => ("deletion", "its files were not confirmed deleted"),
    };
    let missing = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<ApiError>())
        .is_some_and(|api| api.status == StatusCode::NOT_FOUND);
    if missing {
        format!(
            "the server did not confirm {what} of {slug}; {state}: \
             no such unadopted repository (or no such owner) on this forge"
        )
    } else {
        format!("the server did not confirm {what} of {slug}; {state}: {err}")
    }
}

fn confirmed(act: Act, owner: &str, repo: &str) -> String {
    match act {
        Act::Adopt => format!(
            "✓ Server confirmed: {owner}/{repo} is adopted and now a repository owned by {owner}"
        ),
        Act::Delete => {
            format!("✓ Server confirmed: the unadopted files of {owner}/{repo} are deleted")
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn unadopted_act(client: &Client, args: UnadoptedActArgs, act: Act) -> Result<()> {
    let (owner, repo) = crate::api::split_repo(&args.repo)?;
    eprint!("{}", unadopted_preamble(act, owner, repo));
    let question = match act {
        Act::Adopt => "Adopt this repository?",
        Act::Delete => "Permanently delete these files?",
    };
    confirm_typed(args.yes, question, &args.repo)?;
    let result = match act {
        Act::Adopt => admin::unadopted_adopt(client, owner, repo).await,
        Act::Delete => admin::unadopted_delete(client, owner, repo).await,
    };
    if let Err(err) = result {
        let context = not_confirmed(act, &args.repo, &err);
        return Err(err.context(context));
    }
    println!("{}", confirmed(act, owner, repo));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;
    use serde_json::json;

    fn api_error(status: StatusCode, message: &str) -> anyhow::Error {
        anyhow::Error::new(ApiError {
            status,
            url: "https://forge.example/api/v1/admin/x".into(),
            message: message.into(),
            body: String::new(),
        })
    }

    fn create_args(argv: &[&str]) -> Result<UserCreateArgs, clap::Error> {
        let mut full = vec!["fj", "admin", "user", "create"];
        full.extend_from_slice(argv);
        match Cli::try_parse_from(full)?.command {
            Command::Admin(AdminCmd {
                command:
                    AdminSub::User(UserCmd {
                        command: UserSub::Create(args),
                    }),
            }) => Ok(args),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn forbidden_reads_as_requires_an_instance_administrator() {
        let err = admin_context(
            api_error(StatusCode::FORBIDDEN, "user should be the site admin"),
            "forge.example",
        );
        assert_eq!(
            err.to_string(),
            "requires an instance administrator on forge.example \
             (HTTP 403: user should be the site admin)"
        );
        // The typed error survives for the exit-code mapping.
        assert!(err.chain().any(|c| c.downcast_ref::<ApiError>().is_some()));
    }

    #[test]
    fn other_failures_are_not_relabeled_as_admin_denials() {
        for status in [
            StatusCode::NOT_FOUND,
            StatusCode::UNPROCESSABLE_ENTITY,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let err = admin_context(api_error(status, "nope"), "forge.example");
            assert!(
                !err.to_string().contains("instance administrator"),
                "{status}: {err}"
            );
        }
        let plain = admin_context(anyhow!("connection reset"), "forge.example");
        assert_eq!(plain.to_string(), "connection reset");
    }

    #[test]
    fn forbidden_without_a_server_message_still_names_the_requirement() {
        assert_eq!(
            admin_required("forge.example", ""),
            "requires an instance administrator on forge.example (HTTP 403)"
        );
    }

    #[test]
    fn settings_show_server_values_and_label_missing_sections_unavailable() {
        let sections = vec![
            ("api", Section::Available(json!({"max_response_items": 50}))),
            (
                "repository",
                Section::Available(json!({"forks_disabled": false, "lfs_disabled": true})),
            ),
            (
                "ui",
                Section::Available(
                    json!({"allowed_reactions": ["+1", "heart"], "custom_emojis": []}),
                ),
            ),
            ("attachment", Section::Unavailable(404)),
        ];
        let text = render_settings(&sections);
        assert!(text.contains("max_response_items  50"), "{text}");
        assert!(text.contains("lfs_disabled    true"), "{text}");
        assert!(text.contains("allowed_reactions  +1, heart"), "{text}");
        assert!(text.contains("custom_emojis      (none)"), "{text}");
        assert!(
            text.contains("attachment: unavailable on this forge (HTTP 404)"),
            "{text}"
        );
        // A key the server did not send is never invented.
        assert!(!text.contains("mirrors_disabled"), "{text}");
        assert!(!text.contains("max_size"), "{text}");
    }

    #[test]
    fn settings_json_nulls_and_lists_unavailable_sections() {
        let sections = vec![
            ("api", Section::Available(json!({"default_paging_num": 30}))),
            ("ui", Section::Unavailable(404)),
        ];
        assert_eq!(
            settings_json(&sections),
            json!({
                "api": {"default_paging_num": 30},
                "ui": null,
                "unavailable": ["ui"]
            })
        );
    }

    #[test]
    fn empty_settings_object_is_not_padded_with_defaults() {
        let text = render_settings(&[("api", Section::Available(json!({})))]);
        assert_eq!(text, "api:\n  (the server sent no values)\n");
    }

    #[test]
    fn cron_row_marks_missing_fields_unknown_and_zero_time_never() {
        let task: Cron = serde_json::from_value(json!({
            "name": "repo_health_check",
            "schedule": "@midnight",
            "next": "2026-10-09T00:00:00Z",
            "prev": "0001-01-01T00:00:00Z",
            "exec_times": 0
        }))
        .unwrap();
        assert_eq!(
            cron_row(&task),
            vec![
                "repo_health_check",
                "@midnight",
                "2026-10-09T00:00:00Z",
                "never",
                "0"
            ]
        );

        let bare: Cron = serde_json::from_value(json!({"name": "x"})).unwrap();
        assert_eq!(
            cron_row(&bare),
            vec!["x", "unknown", "unknown", "unknown", "unknown"]
        );
    }

    #[test]
    fn create_requires_an_explicit_credential_choice() {
        let err = create_args(&[
            "alice",
            "--email",
            "a@example.com",
            "--send-notify",
            "false",
        ])
        .expect_err("no credential mode");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn create_has_no_plain_password_flag() {
        let err = create_args(&[
            "alice",
            "--email",
            "a@example.com",
            "--password",
            "hunter2",
            "--must-change-password",
            "true",
            "--send-notify",
            "false",
        ])
        .expect_err("--password must not exist");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn create_requires_explicit_change_and_notify_choices() {
        let base = ["alice", "--email", "a@example.com", "--password-stdin"];

        let mut argv = base.to_vec();
        argv.extend(["--send-notify", "false"]);
        assert!(
            create_args(&argv).is_err(),
            "must-change-password is required"
        );

        let mut argv = base.to_vec();
        argv.extend(["--must-change-password", "true"]);
        assert!(create_args(&argv).is_err(), "send-notify is required");

        let mut argv = base.to_vec();
        argv.extend(["--must-change-password", "false", "--send-notify", "true"]);
        let args = create_args(&argv).expect("explicit choices parse");
        assert_eq!(args.must_change_password, Some(false));
        assert!(args.send_notify);
        assert_eq!(credential_source(&args), CredentialSource::Stdin);
    }

    #[test]
    fn create_credential_modes_are_mutually_exclusive() {
        assert!(create_args(&[
            "alice",
            "--email",
            "a@example.com",
            "--password-stdin",
            "--password-prompt",
            "--must-change-password",
            "true",
            "--send-notify",
            "false",
        ])
        .is_err());

        let args = create_args(&[
            "alice",
            "--email",
            "a@example.com",
            "--no-password",
            "--send-notify",
            "true",
        ])
        .expect("no-password parses without must-change-password");
        assert_eq!(credential_source(&args), CredentialSource::None);
        assert_eq!(args.must_change_password, None);
    }

    #[test]
    fn credential_source_refuses_echo_and_unanswerable_prompts() {
        assert!(check_credential_source(CredentialSource::Stdin, true).is_err());
        assert!(check_credential_source(CredentialSource::Prompt, false).is_err());
        assert!(check_credential_source(CredentialSource::Stdin, false).is_ok());
        assert!(check_credential_source(CredentialSource::Prompt, true).is_ok());
        assert!(check_credential_source(CredentialSource::None, false).is_ok());
        assert!(check_credential_source(CredentialSource::None, true).is_ok());
    }

    #[test]
    fn password_from_stdin_drops_one_line_ending_only() {
        assert_eq!(password_from_stdin("s3cret\n").unwrap(), "s3cret");
        assert_eq!(password_from_stdin("s3cret\r\n").unwrap(), "s3cret");
        assert_eq!(password_from_stdin(" pad ded ").unwrap(), " pad ded ");
        assert!(password_from_stdin("").is_err());
        assert!(password_from_stdin("\n").is_err());
        assert!(password_from_stdin("one\ntwo\n").is_err());
    }

    #[test]
    fn created_summary_shows_the_confirmed_account_and_no_credential() {
        let created = json!({
            "id": 7, "login": "alice", "email": "a@example.com",
            "full_name": "Alice A", "is_admin": false
        });
        let text = created_summary(&created, CredentialSource::Stdin, Some(true), false);
        assert!(text.starts_with("✓ Created user alice (id 7)\n"), "{text}");
        assert!(text.contains("email: a@example.com"), "{text}");
        assert!(text.contains("administrator: no"), "{text}");
        assert!(
            text.contains("password read from stdin (not shown)"),
            "{text}"
        );
        assert!(
            text.contains("password change at first sign-in: requested (not confirmed"),
            "{text}"
        );
        assert!(text.contains("notification email: not requested"), "{text}");
    }

    #[test]
    fn created_summary_reports_only_what_the_server_sent() {
        let text = created_summary(&json!({}), CredentialSource::None, None, true);
        assert!(
            text.starts_with("✓ Created user (login not reported)\n"),
            "{text}"
        );
        assert!(!text.contains("\n  email:"), "{text}");
        assert!(!text.contains("administrator:"), "{text}");
        assert!(!text.contains("password change"), "{text}");
        assert!(text.contains("none submitted"), "{text}");
    }

    #[test]
    fn preamble_shows_source_path_and_consequence() {
        let adopt = unadopted_preamble(Act::Adopt, "alice", "legacy");
        assert!(
            adopt.contains("Unadopted repository: alice/legacy"),
            "{adopt}"
        );
        assert!(adopt.contains("Source path: alice/legacy.git"), "{adopt}");
        assert!(adopt.contains("a repository owned by alice"), "{adopt}");

        let delete = unadopted_preamble(Act::Delete, "alice", "legacy");
        assert!(delete.contains("Source path: alice/legacy.git"), "{delete}");
        assert!(
            delete.contains("permanently removes these files from the server"),
            "{delete}"
        );
    }

    #[test]
    fn confirm_gate_never_waits_on_a_non_terminal() {
        assert!(!confirm_gate(true, false).unwrap());
        assert!(!confirm_gate(true, true).unwrap());
        assert!(confirm_gate(false, true).unwrap());
        let err = confirm_gate(false, false).unwrap_err();
        assert!(err.to_string().contains("--yes"), "{err}");
    }

    #[test]
    fn a_failed_adoption_never_reads_as_adopted() {
        let missing = api_error(StatusCode::NOT_FOUND, "");
        let text = not_confirmed(Act::Adopt, "alice/legacy", &missing);
        assert!(text.contains("did not confirm adoption"), "{text}");
        assert!(text.contains("it was not adopted"), "{text}");
        assert!(text.contains("no such unadopted repository"), "{text}");

        let boom = api_error(StatusCode::INTERNAL_SERVER_ERROR, "disk error");
        let text = not_confirmed(Act::Delete, "alice/legacy", &boom);
        assert!(text.contains("did not confirm deletion"), "{text}");
        assert!(text.contains("disk error"), "{text}");
    }

    #[test]
    fn confirmed_wording_is_only_for_the_server_ack() {
        assert_eq!(
            confirmed(Act::Adopt, "alice", "legacy"),
            "✓ Server confirmed: alice/legacy is adopted and now a repository owned by alice"
        );
        assert!(confirmed(Act::Delete, "alice", "legacy").contains("are deleted"));
    }

    #[test]
    fn unadopted_mutations_take_yes_and_list_takes_paging() {
        let cli = Cli::try_parse_from(["fj", "admin", "unadopted", "delete", "a/b", "-y"])
            .expect("delete parses");
        match cli.command {
            Command::Admin(AdminCmd {
                command:
                    AdminSub::Unadopted(UnadoptedCmd {
                        command: UnadoptedSub::Delete(args),
                    }),
            }) => {
                assert!(args.yes);
                assert_eq!(args.repo, "a/b");
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "fj",
            "admin",
            "unadopted",
            "list",
            "--pattern",
            "al*",
            "--page",
            "2",
            "-L",
            "10",
        ])
        .expect("list parses");
        match cli.command {
            Command::Admin(AdminCmd {
                command:
                    AdminSub::Unadopted(UnadoptedCmd {
                        command: UnadoptedSub::List(args),
                    }),
            }) => {
                assert_eq!(args.pattern.as_deref(), Some("al*"));
                assert_eq!((args.page, args.limit), (2, 10));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
