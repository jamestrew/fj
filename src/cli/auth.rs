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
    }
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
        "{} Logged in to {} as {}",
        output::bold("✓"),
        output::bold(&hostname),
        output::bold(&me.login)
    );
    Ok(())
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
            if token_present { "present in keychain" } else { "missing" }
        );
        if args.show_token {
            if let Some(t) = token_store::load_token(name)? {
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
