use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use dialoguer::Password;

use crate::api;
use crate::auth as token_store;
use crate::client::Client;
use crate::config::hosts::{Host, Hosts};
use crate::output;

#[derive(Debug, Args)]
pub struct AuthCmd {
    #[command(subcommand)]
    pub command: AuthSub,
}

#[derive(Debug, Subcommand)]
pub enum AuthSub {
    /// Authenticate with a Forgejo host. Stores the token in your OS keychain.
    Login(LoginArgs),
    /// Show which hosts you're signed in to.
    Status(StatusArgs),
    /// Forget the token for a host.
    Logout(LogoutArgs),
    /// List configured hosts.
    List,
    /// Set the default host for commands that omit `--host`.
    Switch(SwitchArgs),
    /// Print the stored token for a host (for scripting).
    Token(TokenArgs),
    /// Re-verify the stored token. Replaces it if a new one is given.
    Refresh(RefreshArgs),
    /// Install a git credential helper that uses fj's stored token.
    SetupGit(SetupGitArgs),
}

#[derive(Debug, Args)]
pub struct TokenArgs {
    #[arg(long)]
    pub host: Option<String>,
    /// Print the token even if stdout is a terminal (default refuses to, to
    /// avoid accidental shoulder-surfing in shell history).
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct RefreshArgs {
    #[arg(long)]
    pub host: Option<String>,
    /// Replace the stored token. If omitted, fj just re-verifies the existing one.
    #[arg(long)]
    pub token: Option<String>,
    /// Read replacement token from stdin.
    #[arg(long, conflicts_with = "token")]
    pub with_token: bool,
}

#[derive(Debug, Args)]
pub struct SetupGitArgs {
    #[arg(long)]
    pub host: Option<String>,
    /// Print the recommended git config commands without applying them.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Hostname to log in to (e.g. `rasterhub.com`).
    #[arg(long)]
    pub host: Option<String>,
    /// Pass the token as a flag instead of through stdin / interactive prompt.
    #[arg(long, env = "FJ_TOKEN", hide_env_values = true)]
    pub token: Option<String>,
    /// Read the token from stdin (useful for piping `cat token.txt | fj auth login --with-token`).
    #[arg(long, conflicts_with = "token")]
    pub with_token: bool,
    /// Git protocol to use when cloning from this host.
    #[arg(long, value_parser = ["https", "ssh"], default_value = "https")]
    pub git_protocol: String,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Show the token (still doesn't read it from disk; pulled from keychain).
    #[arg(long)]
    pub show_token: bool,
}

#[derive(Debug, Args)]
pub struct LogoutArgs {
    /// Hostname to remove. Required when more than one host is configured.
    #[arg(long)]
    pub host: Option<String>,
}

#[derive(Debug, Args)]
pub struct SwitchArgs {
    /// Hostname to make the new default.
    pub host: String,
}

pub async fn run(cmd: AuthCmd) -> Result<()> {
    match cmd.command {
        AuthSub::Login(args) => login(args).await,
        AuthSub::Status(args) => status(args).await,
        AuthSub::Logout(args) => logout(args),
        AuthSub::List => list(),
        AuthSub::Switch(args) => switch(args),
        AuthSub::Token(args) => token(args),
        AuthSub::Refresh(args) => refresh(args).await,
        AuthSub::SetupGit(args) => setup_git(args),
    }
}

fn token(args: TokenArgs) -> Result<()> {
    use std::io::IsTerminal;
    let hosts = Hosts::load()?;
    let host = hosts.resolve_host(args.host.as_deref())?.to_string();
    let token = token_store::load_token(&host)?
        .ok_or_else(|| anyhow!("no token stored for {host}; run `fj auth login`"))?;
    if std::io::stdout().is_terminal() && !args.force {
        return Err(anyhow!(
            "refusing to print a plaintext token to a terminal; pipe to a file/command, or pass --force"
        ));
    }
    println!("{token}");
    Ok(())
}

async fn refresh(args: RefreshArgs) -> Result<()> {
    let hosts = Hosts::load()?;
    let host = hosts.resolve_host(args.host.as_deref())?.to_string();
    let cfg = hosts
        .hosts
        .get(&host)
        .cloned()
        .ok_or_else(|| anyhow!("host '{host}' not configured"))?;

    let new_token = if let Some(t) = args.token {
        Some(t)
    } else if args.with_token {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        Some(buf.trim().to_string())
    } else {
        None
    };

    let token = match &new_token {
        Some(t) => t.clone(),
        None => token_store::load_token(&host)?
            .ok_or_else(|| anyhow!("no token stored; pass --token to set one"))?,
    };

    let probe = build_probe_client(&host, &cfg, &token)?;
    let me = api::user::current(&probe)
        .await
        .context("verifying token against /api/v1/user")?;

    if new_token.is_some() {
        token_store::store_token(&host, &token)?;
        println!("✓ Replaced token for {host} (verified as {})", me.login);
    } else {
        println!("✓ Token for {host} is still valid (user: {})", me.login);
    }
    Ok(())
}

fn setup_git(args: SetupGitArgs) -> Result<()> {
    let hosts = Hosts::load()?;
    let host = hosts.resolve_host(args.host.as_deref())?.to_string();
    validate_hostname(&host)?;
    let cfg = hosts
        .hosts
        .get(&host)
        .cloned()
        .ok_or_else(|| anyhow!("host '{host}' not configured"))?;
    if let Some(u) = cfg.user.as_deref() {
        validate_username(u)?;
    }

    let key = format!("credential.https://{host}.helper");
    let value = credential_helper_value(&host);

    let user_key = format!("credential.https://{host}.username");
    let user_val = cfg.user.as_deref().unwrap_or("");

    if args.dry_run {
        println!("# Run these commands to install the fj git credential helper:");
        println!("git config --global {key} {value:?}");
        if !user_val.is_empty() {
            println!("git config --global {user_key} {user_val:?}");
        }
        return Ok(());
    }

    // Call `git config` directly with separate args — no shell layer between
    // us and git, so there's nothing to escape.
    run_git_config(&key, &value)?;
    if !user_val.is_empty() {
        run_git_config(&user_key, user_val)?;
    }
    println!("✓ Installed git credential helper for {host}");
    Ok(())
}

fn run_git_config(key: &str, value: &str) -> Result<()> {
    let status = std::process::Command::new("git")
        .args(["config", "--global", key, value])
        .status()
        .context("running `git config`")?;
    if !status.success() {
        return Err(anyhow!(
            "`git config --global {key} ...` failed with status {status}"
        ));
    }
    Ok(())
}

/// Build the `credential.https://<host>.helper` value installed by
/// `setup-git`. Two invariants this function exists to preserve:
///
/// 1. **No positional-arg leakage.** Git appends the operation (`get`,
///    `store`, `erase`) as a positional arg to the helper string. The
///    obvious shape `!fj auth token --host <h> | sed 's/^/password=/'`
///    is broken because `sed` consumes `get` as a filename and bails.
///    Wrapping the pipeline in a shell function `f() { … }; f` makes
///    the trailing positional an arg to `f`, which ignores it.
///
/// 2. **No shell metacharacters in the host.** `validate_hostname`
///    upstream is the keystone here; this function trusts its input.
fn credential_helper_value(host: &str) -> String {
    format!("!f() {{ fj auth token --host {host} | sed 's/^/password=/'; }}; f")
}

/// A hostname must look like a DNS name (optionally with a port). This is the
/// keystone of `setup-git`'s safety: if the host is shell-safe, the
/// credential-helper interpolation below is too. Reject anything else.
fn validate_hostname(host: &str) -> Result<()> {
    if host.is_empty() || host.len() > 253 {
        return Err(anyhow!("invalid hostname '{host}'"));
    }
    let ok = host
        .chars()
        .all(|c| matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | ':'));
    if !ok {
        return Err(anyhow!(
            "hostname '{host}' contains characters that are not allowed in a DNS name"
        ));
    }
    Ok(())
}

fn validate_username(user: &str) -> Result<()> {
    if user.is_empty() || user.len() > 64 {
        return Err(anyhow!("invalid username '{user}'"));
    }
    let ok = user
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        return Err(anyhow!(
            "username '{user}' contains characters that are not allowed in a Forgejo login"
        ));
    }
    Ok(())
}

async fn login(args: LoginArgs) -> Result<()> {
    let hostname = match args.host.clone() {
        Some(h) => h,
        None => prompt_hostname()?,
    };
    let hostname = hostname.trim().to_string();
    if hostname.is_empty() {
        return Err(anyhow!("hostname is required"));
    }

    let token = read_token(&args)?;
    if token.trim().is_empty() {
        return Err(anyhow!("token is required"));
    }

    let git_protocol = args.git_protocol.clone();

    // Verify before persisting: never store a token we can't actually use.
    let provisional = Host {
        user: None,
        git_protocol: git_protocol.clone(),
        api_base_path: None,
    };
    let probe_client = build_probe_client(&hostname, &provisional, &token)?;
    let me = api::user::current(&probe_client)
        .await
        .context("verifying token against /api/v1/user")?;

    // One-shot version probe. Surfaces obvious version mismatches before
    // they manifest as cryptic 404s on later commands. Failure here is
    // non-fatal: some Forgejo-compatible servers may not expose /version
    // identically.
    let version_string = probe_version(&probe_client).await.unwrap_or_default();
    warn_on_old_forgejo(&version_string);

    // Persist host + token.
    let mut hosts = Hosts::load()?;
    let host = Host {
        user: Some(me.login.clone()),
        git_protocol,
        api_base_path: None,
    };
    hosts.upsert(&hostname, host);
    hosts.save()?;
    token_store::store_token(&hostname, &token)?;

    println!(
        "{} Logged in to {} as {}{}",
        output::bold("✓"),
        output::bold(&hostname),
        output::bold(&me.login),
        if version_string.is_empty() {
            String::new()
        } else {
            format!(" ({})", output::dim(&version_string))
        }
    );
    Ok(())
}

async fn probe_version(client: &Client) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct V {
        version: String,
    }
    let v: V = client
        .json(reqwest::Method::GET, "/api/v1/version", &[], None::<&()>)
        .await?;
    Ok(v.version)
}

/// `version_string` looks like "7.0.16+gitea-1.21.11" or "1.21.11". We warn
/// when the leading numeric component looks pre-7.x. Anything we can't parse
/// gets a silent pass since exotic Forgejo-likes still mostly work.
fn warn_on_old_forgejo(version_string: &str) {
    if let Some(msg) = version_warning(version_string) {
        eprintln!("{} {msg}", output::bold("warning:"));
    }
}

/// Pure helper. Returns the human-readable warning (without the "warning:"
/// prefix) when the server's version is older than the fj baseline. Returns
/// `None` when the version is unparseable or fresh enough.
fn version_warning(version_string: &str) -> Option<String> {
    if version_string.is_empty() {
        return None;
    }
    let head = version_string
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .next()
        .unwrap_or("");
    let major: u32 = head
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if major == 0 || major >= 7 {
        return None;
    }
    Some(format!(
        "server reports version `{version_string}`, which is older than the Forgejo 7.x baseline fj is tested against. Some commands (pr ready, repo mirror-sync, search code, milestone) may 404. See docs/compatibility.md."
    ))
}

fn build_probe_client(host: &str, cfg: &Host, token: &str) -> Result<Client> {
    let resolved = crate::client::ResolvedHost {
        name: host.to_string(),
        host: cfg.clone(),
        token: token.to_string(),
    };
    Client::new(resolved)
}

fn read_token(args: &LoginArgs) -> Result<String> {
    if let Some(t) = &args.token {
        return Ok(t.clone());
    }
    if args.with_token {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        return Ok(buf.trim().to_string());
    }
    let t = Password::new()
        .with_prompt("Paste a Forgejo access token")
        .interact()
        .context("reading token")?;
    Ok(t)
}

fn prompt_hostname() -> Result<String> {
    eprint!("Hostname (e.g. rasterhub.com): ");
    use std::io::{stderr, stdin, Write};
    stderr().flush().ok();
    let mut buf = String::new();
    stdin().read_line(&mut buf).context("reading hostname")?;
    Ok(buf)
}

async fn status(args: StatusArgs) -> Result<()> {
    let hosts = Hosts::load()?;
    if hosts.hosts.is_empty() {
        println!("You are not signed in to any Forgejo hosts. Run `fj auth login`.");
        return Ok(());
    }
    for (name, host) in &hosts.hosts {
        let current_marker = if hosts.current.as_deref() == Some(name) {
            output::bold(" (current)")
        } else {
            String::new()
        };
        println!("{}{current_marker}", output::bold(name));
        let token_present = token_store::load_token(name)?.is_some();
        match host.user.as_deref() {
            Some(u) => println!("  ✓ Logged in as {u}"),
            None => println!("  ✓ Host configured"),
        }
        println!(
            "  ✓ Token: {}",
            if token_present {
                "present in keychain"
            } else {
                "missing"
            }
        );
        if args.show_token {
            use std::io::IsTerminal;
            if std::io::stdout().is_terminal() {
                println!(
                    "  Token value: {}",
                    output::dim("(hidden; pipe to a file or use `fj auth token --force`)")
                );
            } else if let Some(t) = token_store::load_token(name)? {
                println!("  Token value: {t}");
            }
        }
        println!("  Git protocol: {}", host.git_protocol);
    }
    Ok(())
}

fn logout(args: LogoutArgs) -> Result<()> {
    let mut hosts = Hosts::load()?;
    let target = match args.host {
        Some(h) => h,
        None => {
            if hosts.hosts.len() != 1 {
                return Err(anyhow!(
                    "multiple hosts configured; pass `--host <hostname>`"
                ));
            }
            hosts.hosts.keys().next().cloned().unwrap()
        }
    };
    if hosts.remove(&target).is_none() {
        return Err(anyhow!("no entry for host '{target}'"));
    }
    hosts.save()?;
    token_store::delete_token(&target)?;
    println!("✓ Logged out of {target}");
    Ok(())
}

fn list() -> Result<()> {
    let hosts = Hosts::load()?;
    if hosts.hosts.is_empty() {
        println!("(no hosts configured)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = hosts
        .hosts
        .iter()
        .map(|(name, h)| {
            let current = if hosts.current.as_deref() == Some(name) {
                "*"
            } else {
                ""
            };
            vec![
                current.into(),
                name.clone(),
                h.user.clone().unwrap_or_default(),
                h.git_protocol.clone(),
            ]
        })
        .collect();
    let table = output::render_table(&["", "HOST", "USER", "PROTOCOL"], &rows);
    print!("{table}");
    Ok(())
}

fn switch(args: SwitchArgs) -> Result<()> {
    let mut hosts = Hosts::load()?;
    hosts.set_current(&args.host)?;
    hosts.save()?;
    println!("✓ Default host is now {}", args.host);
    Ok(())
}

#[cfg(test)]
mod setup_git_tests {
    use super::*;

    #[test]
    fn rejects_shell_metacharacters_in_hostname() {
        assert!(validate_hostname("rasterhub.com; rm -rf /").is_err());
        assert!(validate_hostname("foo`whoami`").is_err());
        assert!(validate_hostname("foo $(whoami)").is_err());
        assert!(validate_hostname("foo|bar").is_err());
        assert!(validate_hostname("foo&bar").is_err());
        assert!(validate_hostname("foo\"bar").is_err());
        assert!(validate_hostname("foo'bar").is_err());
    }

    #[test]
    fn accepts_normal_hostnames() {
        assert!(validate_hostname("rasterhub.com").is_ok());
        assert!(validate_hostname("git.example.org:2222").is_ok());
        assert!(validate_hostname("forgejo-1.internal").is_ok());
    }

    #[test]
    fn rejects_shell_metacharacters_in_username() {
        assert!(validate_username("alice; rm").is_err());
        assert!(validate_username("alice$user").is_err());
    }

    #[test]
    fn accepts_normal_usernames() {
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("alice.bob_123").is_ok());
    }

    /// Regression test for issue #1: git appends the operation name
    /// (`get` / `store` / `erase`) as a positional arg to the helper
    /// string. Without a function wrapper, `sed` interprets `get` as a
    /// filename and the entire credential dance breaks.
    #[test]
    fn credential_helper_wraps_in_shell_function() {
        let v = credential_helper_value("rasterhub.com");
        // Begins with the `!`-prefix git uses to invoke via sh.
        assert!(v.starts_with('!'), "got: {v}");
        // Defines a function before invoking it.
        assert!(v.contains("f() {"), "got: {v}");
        assert!(v.ends_with("}; f"), "got: {v}");
        // sed call is inside the function body, not at the tail.
        let after_sed = v.split("sed").nth(1).unwrap_or("");
        assert!(
            after_sed.contains("}; f"),
            "sed must be inside the function so trailing `get` is ignored; got: {v}"
        );
        // Host is interpolated correctly.
        assert!(v.contains("--host rasterhub.com"), "got: {v}");
    }

    /// Simulate git's invocation: pretend a shell evaluates the helper
    /// with `get` as $1 and check that the pipeline structure isolates
    /// it. (We don't actually exec a shell here; we inspect the string
    /// to assert that the trailing token after the closing brace is the
    /// function call, not a sed-consumable positional.)
    #[test]
    fn credential_helper_isolates_git_verb() {
        for op in ["get", "store", "erase"] {
            // The function call `f` is the only thing after the closing
            // brace, so `f $op` becomes the invocation. `f` ignores its
            // args, so the verb is swallowed harmlessly.
            let v = credential_helper_value("h.example");
            let simulated = format!("{v} {op}");
            // The simulated shell line still ends with `f <op>`, never
            // with `sed ... <op>`.
            assert!(
                simulated.ends_with(&format!("f {op}")),
                "verb must trail `f`, not sed; got: {simulated}"
            );
        }
    }

    #[test]
    fn version_warning_fires_on_old() {
        assert!(version_warning("1.21.11").is_some());
        assert!(version_warning("5.0.0").is_some());
        assert!(version_warning("6.0.0+gitea-1.20.0").is_some());
    }

    #[test]
    fn version_warning_silent_on_modern() {
        assert!(version_warning("7.0.16+gitea-1.21.11").is_none());
        assert!(version_warning("8.0.0").is_none());
        assert!(version_warning("10.5.2-beta").is_none());
    }

    #[test]
    fn version_warning_silent_on_unparseable() {
        assert!(version_warning("").is_none());
        assert!(version_warning("totally-unknown").is_none());
        // Hardcoded 0.x major never triggers the warning either (likely a
        // dev / preview build we don't want to be noisy about).
        assert!(version_warning("0.99.0").is_none());
    }
}
