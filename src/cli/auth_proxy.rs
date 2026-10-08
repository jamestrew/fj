//! `fj auth proxy`: credentials for a reverse proxy that guards the forge with
//! HTTP Basic authentication.
//!
//! The proxy credential is a second secret, separate from the forge token. The
//! username lives in hosts.toml; the password goes to the token store under
//! its own key (`auth::proxy_keychain_key`) and is never printed.

use anyhow::{anyhow, Context, Result};
use clap::Args;
use url::Url;

use crate::auth as token_store;
use crate::client::proxy::transport_is_safe;
use crate::client::PatTransport;
use crate::config::hosts::{AuthKind, Host, Hosts, ProxyConfig};
use crate::output;

/// Appended to `fj auth proxy --help`. The transport tradeoff is stated here
/// because it is a decision the user makes, not an implementation detail.
pub const LONG_HELP: &str = "\
The proxy credential is separate from your forge token and is stored like it:
the username in hosts.toml, the password in the OS keychain (or the 0600 file
fallback). In CI, set FJ_PROXY_USER and FJ_PROXY_PASSWORD instead (or the
host-scoped FJ_PROXY_USER_<HOST> / FJ_PROXY_PASSWORD_<HOST>, same suffix as
FJ_TOKEN_<HOST>). fj sends the credential only after the proxy asks for it,
only to the configured host, and only over HTTPS or to a loopback address.

A proxy that answers 407 reads Proxy-Authorization, so nothing conflicts.

A proxy that answers 401 with `WWW-Authenticate: Basic` (nginx auth_basic,
Caddy basic_auth, Traefik basicAuth) reads the Authorization header, which is
also where the forge expects its token. One request has one Authorization
header, so the token has to travel another way. Pick with --pat-transport:

  header (default)  The token goes in `X-Forge-Authorization`. It stays out of
                    URLs and access logs, but the proxy needs one line to hand
                    it to the forge. nginx:
                      proxy_set_header Authorization $http_x_forge_authorization;
                    Caddy (inside reverse_proxy):
                      header_up Authorization {http.request.header.X-Forge-Authorization}

  query             The token goes in the URL as `?token=`. Works with an
                    unmodified proxy, but the token is written to the proxy's
                    and the forge's access logs, and it only works while the
                    forge still accepts query tokens (Forgejo/Gitea setting
                    DISABLE_QUERY_AUTH_TOKEN, which newer servers turn on).
                    Asset uploads are refused in this mode. fj redacts the
                    token from its own --debug output and errors either way.

Git over HTTPS is out of scope: git and `fj auth setup-git` hit the same
header conflict and git cannot send a second header per host without putting
a secret in its config. Use SSH for git on a proxied host
(`fj auth login --git-protocol ssh`).";

#[derive(Debug, Args)]
pub struct ProxyArgs {
    /// Host the proxy sits in front of (defaults to the current host). May be
    /// set up before `fj auth login`, which itself has to get through the proxy.
    #[arg(long)]
    pub host: Option<String>,
    /// HTTP Basic username the proxy expects.
    #[arg(long)]
    pub username: Option<String>,
    /// Read the proxy password from stdin instead of prompting.
    #[arg(long)]
    pub with_password: bool,
    /// How the forge token travels when the proxy takes the Authorization
    /// header: `header` (secure, needs one proxy config line) or `query`
    /// (works with a stock proxy, leaks the token into access logs).
    #[arg(long, value_parser = ["header", "query"], value_name = "MODE")]
    pub pat_transport: Option<String>,
    /// Forget the proxy credential for the host.
    #[arg(long, conflicts_with_all = ["username", "with_password", "pat_transport"])]
    pub clear: bool,
}

/// What an invocation asks for, decided from the flags and the existing
/// config alone.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Clear,
    /// Change only how the token travels; the stored password is kept.
    SetTransport(PatTransport),
    /// Store a (new) password for `username`.
    Store {
        username: String,
        pat_transport: PatTransport,
    },
}

fn parse_transport(raw: &str) -> PatTransport {
    match raw {
        "query" => PatTransport::Query,
        _ => PatTransport::Header,
    }
}

/// HTTP Basic joins user and password with the first `:`, so a username
/// containing one would silently shift part of itself into the password.
fn validate_proxy_username(user: &str) -> Result<()> {
    if user.is_empty() {
        return Err(anyhow!("the proxy username is empty"));
    }
    if user.contains(':') {
        return Err(anyhow!(
            "the proxy username must not contain ':' (HTTP Basic uses it as the separator)"
        ));
    }
    if user.chars().any(char::is_control) {
        return Err(anyhow!("the proxy username contains a control character"));
    }
    Ok(())
}

fn decide(args: &ProxyArgs, existing: Option<&ProxyConfig>) -> Result<Action> {
    if args.clear {
        return Ok(Action::Clear);
    }
    let requested = args.pat_transport.as_deref().map(parse_transport);
    // `--pat-transport` alone on an already-configured host switches the
    // transport without asking for the password again.
    if let (Some(transport), Some(_), None, false) = (
        requested,
        existing,
        args.username.as_ref(),
        args.with_password,
    ) {
        return Ok(Action::SetTransport(transport));
    }
    let username = args
        .username
        .clone()
        .or_else(|| existing.map(|p| p.username.clone()))
        .ok_or_else(|| anyhow!("pass the proxy username with --username <user>"))?;
    let username = username.trim().to_string();
    validate_proxy_username(&username)?;
    Ok(Action::Store {
        username,
        pat_transport: requested
            .or_else(|| existing.map(|p| p.pat_transport))
            .unwrap_or_default(),
    })
}

/// The web origin a host entry points at, for the cleartext check.
fn host_base(name: &str, host: &Host) -> Result<Url> {
    let base = host
        .web_url
        .clone()
        .unwrap_or_else(|| format!("https://{name}"));
    Url::parse(&base).with_context(|| format!("constructing URL for {base}"))
}

/// Refuse to set up a credential fj would never send. Basic is base64, not
/// encryption, so it only goes over HTTPS or to this machine.
fn ensure_safe_transport(name: &str, host: &Host) -> Result<()> {
    if transport_is_safe(&host_base(name, host)?) {
        return Ok(());
    }
    Err(anyhow!(
        "{name} is configured over cleartext HTTP. fj only sends a proxy credential over HTTPS \
         or to a loopback address, so there is nothing to set up for this host."
    ))
}

/// A password read from stdin: one trailing line ending is dropped, nothing
/// else, because a password may legitimately start or end with a space.
fn password_from_stdin_text(text: &str) -> &str {
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.strip_suffix('\r').unwrap_or(text)
}

fn read_password(args: &ProxyArgs, host: &str, username: &str) -> Result<String> {
    use std::io::IsTerminal;
    let password = if args.with_password {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        password_from_stdin_text(&buf).to_string()
    } else if std::io::stdin().is_terminal() {
        dialoguer::Password::new()
            .with_prompt(format!("Proxy password for {username} at {host}"))
            .interact()
            .context("reading proxy password")?
    } else {
        return Err(anyhow!(
            "no terminal to prompt on: pipe the proxy password with --with-password, or set \
             FJ_PROXY_USER and FJ_PROXY_PASSWORD in the environment and skip this command"
        ));
    };
    if password.is_empty() {
        return Err(anyhow!("empty proxy password; nothing stored"));
    }
    Ok(password)
}

/// What to tell the user about the transport they ended up with.
fn transport_note(host: &str, transport: PatTransport) -> String {
    match transport {
        PatTransport::Header => format!(
            "If the proxy answers 401 (not 407), it must forward the token header to the \
             forge. nginx: `proxy_set_header Authorization $http_x_forge_authorization;`. \
             See `fj auth proxy --help`. Then verify with `fj auth refresh --host {host}`."
        ),
        PatTransport::Query => format!(
            "Query transport: behind a 401 proxy your forge token travels in the URL and will \
             appear in the proxy's and the forge's access logs. Prefer the header transport \
             if you can change the proxy config. Verify with `fj auth refresh --host {host}`."
        ),
    }
}

pub(crate) fn run(args: ProxyArgs) -> Result<()> {
    let mut hosts = Hosts::load()?;
    // The host may not be configured yet: the proxy has to be answered before
    // `fj auth login` can verify a token through it.
    let (name, created) = match args.host.as_deref() {
        Some(raw) if !hosts.hosts.contains_key(raw) => {
            let (key, web_url) = super::auth_login::normalize_pat_host(raw)?;
            if hosts.hosts.contains_key(&key) {
                (key, false)
            } else if args.clear {
                return Err(anyhow!("no entry for host '{key}'"));
            } else {
                hosts.upsert(
                    &key,
                    Host {
                        git_protocol: "https".into(),
                        web_url,
                        ..Default::default()
                    },
                );
                (key, true)
            }
        }
        explicit => (hosts.resolve_host(explicit)?.to_string(), false),
    };
    let entry = hosts
        .hosts
        .get(&name)
        .cloned()
        .ok_or_else(|| anyhow!("host '{name}' not configured"))?;
    if entry.auth_kind != AuthKind::Pat {
        return Err(anyhow!(
            "`fj auth proxy` is for hosts fj talks to directly. A Fjord Account host is reached \
             through the Fjord gateway, which a reverse proxy credential does not apply to."
        ));
    }

    match decide(&args, entry.proxy.as_ref())? {
        Action::Clear => {
            token_store::delete_proxy_password(&name)?;
            if let Some(h) = hosts.hosts.get_mut(&name) {
                h.proxy = None;
            }
            hosts.save()?;
            println!("✓ Cleared the reverse proxy credential for {name}");
        }
        Action::SetTransport(pat_transport) => {
            if let Some(proxy) = hosts.hosts.get_mut(&name).and_then(|h| h.proxy.as_mut()) {
                proxy.pat_transport = pat_transport;
            }
            hosts.save()?;
            println!(
                "✓ Token transport for {name} is now `{}`",
                pat_transport.as_str()
            );
            println!("  {}", output::dim(&transport_note(&name, pat_transport)));
        }
        Action::Store {
            username,
            pat_transport,
        } => {
            ensure_safe_transport(&name, &entry)?;
            let password = read_password(&args, &name, &username)?;
            let storage = token_store::store_proxy_password(&name, &password)?;
            if let Some(h) = hosts.hosts.get_mut(&name) {
                h.proxy = Some(ProxyConfig {
                    username: username.clone(),
                    pat_transport,
                });
            }
            hosts.save()?;
            if let token_store::TokenStoreLocation::File(path) = storage {
                println!(
                    "! Keychain unavailable; stored the proxy password in {}",
                    path.display()
                );
            }
            println!(
                "✓ Stored the reverse proxy credential for {name} (user {username}, token \
                 transport: {})",
                pat_transport.as_str()
            );
            println!("  {}", output::dim(&transport_note(&name, pat_transport)));
            if created {
                println!(
                    "  {}",
                    output::dim(&format!("Next: `fj auth login --host {name}`."))
                );
            }
        }
    }
    Ok(())
}

/// The `fj auth status` lines for what was recorded about the server and for
/// a configured reverse proxy. Shows the proxy username and transport, and
/// only whether a password is available: the password itself never reaches a
/// string that gets printed.
pub(crate) fn server_and_proxy_lines(host: &Host, proxy_credential_complete: bool) -> Vec<String> {
    use crate::cli::auth_probe::server_label;
    use crate::client::{Flavor, ServerInfo};
    let mut lines = Vec::new();
    if host.auth_kind == AuthKind::Pat {
        let server = match host.flavor.as_deref() {
            Some(flavor) => server_label(&ServerInfo::new(
                Flavor::parse(flavor),
                host.server_version.as_deref().unwrap_or(""),
            )),
            None => "not recorded (run `fj auth refresh`)".to_string(),
        };
        lines.push(format!("Server: {server}"));
    }
    if let Some(proxy) = host.proxy.as_ref() {
        lines.push(format!(
            "Reverse proxy: user {} (token transport: {}), password {}",
            proxy.username,
            proxy.pat_transport.as_str(),
            if proxy_credential_complete {
                "present"
            } else {
                "missing (run `fj auth proxy`)"
            }
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> ProxyArgs {
        ProxyArgs {
            host: None,
            username: None,
            with_password: false,
            pat_transport: None,
            clear: false,
        }
    }

    fn existing(transport: PatTransport) -> ProxyConfig {
        ProxyConfig {
            username: "gate".into(),
            pat_transport: transport,
        }
    }

    #[test]
    fn first_setup_needs_a_username_and_defaults_to_the_header_transport() {
        assert!(decide(&args(), None).is_err());
        let got = decide(
            &ProxyArgs {
                username: Some(" gate ".into()),
                ..args()
            },
            None,
        )
        .unwrap();
        assert_eq!(
            got,
            Action::Store {
                username: "gate".into(),
                pat_transport: PatTransport::Header
            }
        );
    }

    #[test]
    fn query_transport_is_only_ever_opt_in() {
        let got = decide(
            &ProxyArgs {
                username: Some("gate".into()),
                pat_transport: Some("query".into()),
                ..args()
            },
            None,
        )
        .unwrap();
        assert_eq!(
            got,
            Action::Store {
                username: "gate".into(),
                pat_transport: PatTransport::Query
            }
        );
    }

    #[test]
    fn transport_flag_alone_switches_without_asking_for_the_password() {
        let cfg = existing(PatTransport::Query);
        let got = decide(
            &ProxyArgs {
                pat_transport: Some("header".into()),
                ..args()
            },
            Some(&cfg),
        )
        .unwrap();
        assert_eq!(got, Action::SetTransport(PatTransport::Header));
    }

    #[test]
    fn rotating_the_password_keeps_username_and_transport() {
        let cfg = existing(PatTransport::Query);
        let got = decide(
            &ProxyArgs {
                with_password: true,
                ..args()
            },
            Some(&cfg),
        )
        .unwrap();
        assert_eq!(
            got,
            Action::Store {
                username: "gate".into(),
                pat_transport: PatTransport::Query
            }
        );
    }

    #[test]
    fn clear_wins() {
        let cfg = existing(PatTransport::Header);
        let got = decide(
            &ProxyArgs {
                clear: true,
                ..args()
            },
            Some(&cfg),
        )
        .unwrap();
        assert_eq!(got, Action::Clear);
    }

    #[test]
    fn username_rules() {
        assert!(validate_proxy_username("gate").is_ok());
        assert!(validate_proxy_username("ops@example.com").is_ok());
        assert!(validate_proxy_username("").is_err());
        assert!(validate_proxy_username("a:b").is_err());
        assert!(validate_proxy_username("a\nb").is_err());
    }

    #[test]
    fn cleartext_remote_hosts_are_refused_and_loopback_is_allowed() {
        let https = Host::default();
        assert!(ensure_safe_transport("forge.example", &https).is_ok());

        let cleartext = Host {
            web_url: Some("http://forge.example".into()),
            ..Default::default()
        };
        let err = ensure_safe_transport("forge.example", &cleartext).unwrap_err();
        assert!(err.to_string().contains("cleartext HTTP"));

        let local = Host {
            web_url: Some("http://localhost:3000".into()),
            ..Default::default()
        };
        assert!(ensure_safe_transport("localhost:3000", &local).is_ok());
    }

    #[test]
    fn stdin_password_keeps_everything_but_the_line_ending() {
        assert_eq!(password_from_stdin_text("s3cret\n"), "s3cret");
        assert_eq!(password_from_stdin_text("s3cret\r\n"), "s3cret");
        assert_eq!(password_from_stdin_text(" pad \n"), " pad ");
        assert_eq!(password_from_stdin_text("s3cret"), "s3cret");
    }

    #[test]
    fn status_lines_show_username_and_transport_but_no_secret() {
        let host = Host {
            flavor: Some("gitea".into()),
            server_version: Some("1.27.3".into()),
            proxy: Some(existing(PatTransport::Header)),
            ..Default::default()
        };
        let lines = server_and_proxy_lines(&host, true);
        assert_eq!(lines[0], "Server: Gitea 1.27.3");
        assert_eq!(
            lines[1],
            "Reverse proxy: user gate (token transport: header), password present"
        );
        let missing = server_and_proxy_lines(&host, false);
        assert!(missing[1].contains("password missing"));
    }

    #[test]
    fn status_lines_for_an_entry_written_before_the_probe_existed() {
        let lines = server_and_proxy_lines(&Host::default(), false);
        assert_eq!(lines, vec!["Server: not recorded (run `fj auth refresh`)"]);
    }

    #[test]
    fn help_states_the_tradeoff_and_the_git_scope() {
        assert!(LONG_HELP.contains("X-Forge-Authorization"));
        assert!(LONG_HELP.contains("access logs"));
        assert!(LONG_HELP.contains("DISABLE_QUERY_AUTH_TOKEN"));
        assert!(LONG_HELP.contains("Git over HTTPS is out of scope"));
    }
}
