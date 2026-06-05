use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::auth as token_store;
use crate::cli::auth_probe::build_probe_client;
use crate::cli::{auth_login, auth_setup_git};
use crate::config::hosts::Hosts;
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
    /// Forgejo host whose token to print (defaults to the current host)
    #[arg(long)]
    pub host: Option<String>,
    /// Print the token even if stdout is a terminal (default refuses to, to
    /// avoid accidental shoulder-surfing in shell history).
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct RefreshArgs {
    /// Forgejo host whose token to re-verify (defaults to the current host)
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
    /// Forgejo host to configure the credential helper for (defaults to the current host)
    #[arg(long)]
    pub host: Option<String>,
    /// Print the recommended git config commands without applying them.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Hostname to log in to (e.g. `rasterhub.com`). Ignored in --fjord
    /// mode; the platform URL is used as the host identity instead.
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
    /// Sign in with Fjord Account credentials (email + password + optional
    /// TOTP) rather than a Forgejo personal access token. The resulting
    /// session bearer is used to talk to every Forgejo instance owned by
    /// this account, via the fjord-platform gateway. Use `fj instances`
    /// after signing in to list and pick an instance.
    #[arg(long, conflicts_with_all = ["token", "with_token"])]
    pub fjord: bool,
    /// fjord-platform deployment URL to sign in against. Defaults to the
    /// production deployment. Use this for local dev (`http://localhost:5180`)
    /// or to point at a staging environment.
    #[arg(long, requires = "fjord", value_name = "URL")]
    pub platform_url: Option<String>,
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
        AuthSub::Login(args) => auth_login::login(args).await,
        AuthSub::Status(args) => status(args).await,
        AuthSub::Logout(args) => logout(args),
        AuthSub::List => list(),
        AuthSub::Switch(args) => switch(args),
        AuthSub::Token(args) => token(args),
        AuthSub::Refresh(args) => refresh(args).await,
        AuthSub::SetupGit(args) => auth_setup_git::setup_git(args),
    }
}

fn token(args: TokenArgs) -> Result<()> {
    use std::io::IsTerminal;
    let hosts = Hosts::load()?;
    let host_name = hosts.resolve_host(args.host.as_deref())?.to_string();
    let cfg = hosts
        .hosts
        .get(&host_name)
        .cloned()
        .ok_or_else(|| anyhow!("host '{host_name}' not configured"))?;
    let token = match cfg.auth_kind {
        crate::config::hosts::AuthKind::Pat => token_store::load_token(&host_name)?
            .ok_or_else(|| anyhow!("no token stored for {host_name}; run `fj auth login`"))?,
        crate::config::hosts::AuthKind::FjordAccount => {
            let fa = cfg.fjord_account.as_ref().ok_or_else(|| {
                anyhow!("host '{host_name}' is fjord_account but missing fjord_account config")
            })?;
            token_store::load_fjord_bearer(&fa.platform_url)?.ok_or_else(|| {
                anyhow!(
                    "no Fjord Account session stored for {}; run `fj auth login --fjord`",
                    fa.platform_url
                )
            })?
        }
    };
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

    // `fj auth refresh` is a PAT-mode workflow ("paste me a fresh PAT").
    // Fjord-Account sessions are refreshed by re-running `fj auth login --fjord`.
    if matches!(cfg.auth_kind, crate::config::hosts::AuthKind::FjordAccount) {
        return Err(anyhow!(
            "`fj auth refresh` is for Forgejo PAT hosts. For a Fjord Account session, \
             re-run `fj auth login --fjord` instead."
        ));
    }

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
        let token_present = match host.auth_kind {
            crate::config::hosts::AuthKind::Pat => token_store::load_token(name)?.is_some(),
            crate::config::hosts::AuthKind::FjordAccount => host
                .fjord_account
                .as_ref()
                .map(|fa| token_store::load_fjord_bearer(&fa.platform_url))
                .transpose()?
                .flatten()
                .is_some(),
        };
        let scheme_label = match host.auth_kind {
            crate::config::hosts::AuthKind::Pat => "Forgejo PAT",
            crate::config::hosts::AuthKind::FjordAccount => "Fjord Account",
        };
        println!("  Auth: {scheme_label}");
        match host.user.as_deref() {
            Some(u) => println!("  ✓ Logged in as {u}"),
            None => println!("  ✓ Host configured"),
        }
        if let Some(fa) = host.fjord_account.as_ref() {
            println!("  Platform URL: {}", fa.platform_url);
            match fa.default_instance_id.as_deref() {
                Some(id) => {
                    let display = fa.default_instance_display_name.as_deref().unwrap_or("");
                    println!(
                        "  Default instance: {id}{}",
                        if display.is_empty() {
                            String::new()
                        } else {
                            format!(" ({display})")
                        }
                    );
                }
                None => println!(
                    "  Default instance: {}",
                    output::dim("none (run `fj instances list` then `fj instances use <id>`)")
                ),
            }
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
            } else {
                let t = match host.auth_kind {
                    crate::config::hosts::AuthKind::Pat => token_store::load_token(name)?,
                    crate::config::hosts::AuthKind::FjordAccount => host
                        .fjord_account
                        .as_ref()
                        .and_then(|fa| token_store::load_fjord_bearer(&fa.platform_url).transpose())
                        .transpose()?,
                };
                if let Some(t) = t {
                    println!("  Token value: {t}");
                }
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
    let removed = match hosts.remove(&target) {
        Some(h) => h,
        None => return Err(anyhow!("no entry for host '{target}'")),
    };
    hosts.save()?;
    // Clear the right keychain slot for the auth scheme we just removed.
    match removed.auth_kind {
        crate::config::hosts::AuthKind::Pat => {
            token_store::delete_token(&target)?;
        }
        crate::config::hosts::AuthKind::FjordAccount => {
            if let Some(fa) = removed.fjord_account.as_ref() {
                token_store::delete_fjord_bearer(&fa.platform_url)?;
            }
        }
    }
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
            let scheme = match h.auth_kind {
                crate::config::hosts::AuthKind::Pat => "pat",
                crate::config::hosts::AuthKind::FjordAccount => "fjord-account",
            };
            vec![
                current.into(),
                name.clone(),
                h.user.clone().unwrap_or_default(),
                scheme.into(),
                h.git_protocol.clone(),
            ]
        })
        .collect();
    let table = output::render_table(&["", "HOST", "USER", "AUTH", "PROTOCOL"], &rows);
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
