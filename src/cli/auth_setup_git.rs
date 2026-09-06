//! `fj auth setup-git` — install a git credential helper backed by fj's
//! stored token. The hostname/username validation here is the security
//! keystone; the credential-helper string is shell-evaluated by git.

use anyhow::{anyhow, Context, Result};

use crate::config::hosts::Hosts;

pub(crate) fn setup_git(args: super::auth::SetupGitArgs) -> Result<()> {
    let hosts = Hosts::load()?;
    let host = hosts.resolve_host(args.host.as_deref())?.to_string();
    validate_hostname(&host)?;
    let cfg = hosts
        .hosts
        .get(&host)
        .cloned()
        .ok_or_else(|| anyhow!("host '{host}' not configured"))?;
    if matches!(cfg.auth_kind, crate::config::hosts::AuthKind::FjordAccount) {
        return Err(anyhow!(
            "`fj auth setup-git` is for Forgejo PAT hosts. Fjord Account git credential \
             helpers are not yet implemented (git pushes don't currently flow through the gateway)."
        ));
    }
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
    let _foreground = crate::interrupt::ForegroundChild::enter();
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
pub(crate) fn validate_hostname(host: &str) -> Result<()> {
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
    // A real DNS name or `host:port` never starts or ends with a separator. A
    // leading '-' is the dangerous case: the host is interpolated into
    // `fj auth token --host <host>` inside the shell credential helper, where a
    // '-'-leading value is parsed as a flag, not a hostname. That is the
    // argument-injection class the rest of the CLI guards against, so reject it
    // here (the keystone validation) rather than let it reach the helper.
    let separator = |c: char| matches!(c, '-' | '.' | ':');
    if host.starts_with(separator) || host.ends_with(separator) {
        return Err(anyhow!(
            "invalid hostname '{host}': must not start or end with '-', '.', or ':'"
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
    fn rejects_leading_or_trailing_separator() {
        // A leading '-' would be parsed as a flag when interpolated into
        // `fj auth token --host <host>` in the credential helper.
        assert!(validate_hostname("--data-dir=x").is_err());
        assert!(validate_hostname("-h").is_err());
        // Other malformed edges.
        assert!(validate_hostname("h-").is_err());
        assert!(validate_hostname(".example.com").is_err());
        assert!(validate_hostname("example.com.").is_err());
        assert!(validate_hostname(":8080").is_err());
        assert!(validate_hostname("host:").is_err());
        // Mid-string separators stay valid.
        assert!(validate_hostname("a-b.example.com:443").is_ok());
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

    /// Behavioral test for issue #1. Builds a tempdir containing a stub
    /// `fj` script, then invokes the helper string through `sh -c '… "$@"' sh
    /// <verb>` — the exact pattern git uses when invoking a `!`-prefixed
    /// credential.helper. Asserts stdout is `password=<expected>` and that
    /// no error leaks through for any of the three credential verbs.
    #[test]
    #[cfg(unix)]
    fn credential_helper_runs_through_sh_with_each_git_verb() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");

        // Stub fj that prints a known token, used as the `fj auth token`
        // call inside the helper. We don't need it to parse args; the
        // helper pipes its stdout into sed.
        let stub = dir.path().join("fj");
        fs::write(
            &stub,
            "#!/bin/sh\n# stub fj: print a fixed token regardless of args\necho 'TOKEN-XXX'\n",
        )
        .expect("write stub");
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("chmod");

        // Strip the leading `!` (that's the git-credential-helper sigil,
        // not part of the shell command).
        let raw = credential_helper_value("rasterhub.com");
        let cmd = raw.strip_prefix('!').expect("`!` prefix").to_string();

        // Mirror git's exact invocation: `sh -c '<cmd> "$@"' sh <verb>`.
        let path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );

        for verb in ["get", "store", "erase"] {
            let out = Command::new("sh")
                .args(["-c", &format!("{cmd} \"$@\""), "sh", verb])
                .env("PATH", &path)
                .output()
                .expect("spawn sh");
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success(),
                "helper failed for verb {verb}: stdout={stdout:?} stderr={stderr:?}"
            );
            assert_eq!(
                stdout.trim_end(),
                "password=TOKEN-XXX",
                "unexpected stdout for verb {verb}: {stdout:?} (stderr: {stderr:?})"
            );
            assert!(
                !stderr.contains("No such file or directory"),
                "verb leaked to sed for {verb}: stderr={stderr:?}"
            );
        }
    }
}
