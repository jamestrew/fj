use super::*;
use anyhow::Context;

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cli: Cli) -> Result<()> {
    let auth_command = matches!(cli.command, Command::Auth(_));
    crate::client::account::initialize(if auth_command {
        None
    } else {
        cli.account.clone()
    })?;
    if cli.account.is_some() && matches!(cli.command, Command::Instances(_)) {
        anyhow::bail!("account profiles for Fjord gateway sessions are not supported yet");
    }
    // Before anything that can park this thread in a syscall. The select
    // below is the graceful path; this is the backstop for when the command
    // future stops being pollable and the select never gets another look in.
    // See `crate::interrupt`.
    crate::interrupt::arm();

    crate::client::set_debug(cli.debug);
    crate::output::set_color_choice(cli.color, config::no_color_enabled());
    crate::output::set_json_fields(cli.json_fields.as_deref());
    // Page only the commands whose output can genuinely run long (see
    // `paging::should_page`). Pass `cli.no_pager` through explicitly so we
    // don't have to mutate the process environment to opt out, which would be
    // a process-wide side effect and an unsafe set_var.
    let _pager = if paging::should_page(&cli.command) {
        crate::output::pager::maybe_start(cli.no_pager)
    } else {
        None
    };

    // Run the command, racing it against Ctrl+C. On SIGINT we return an error
    // and let `_pager` drop naturally — its Drop impl restores stdout and
    // waits on the pager child, leaving the terminal in a clean state.
    let cli_host = cli.host.clone();
    let cmd_future = async {
        match cli.command {
            Command::Auth(cmd) => profiles::run(cmd, cli.account, cli_host.as_deref()).await,
            command => dispatch(command, cli_host.as_deref()).await,
        }
    };
    tokio::select! {
        biased;
        sig = tokio::signal::ctrl_c() => {
            sig.context("installing SIGINT handler")?;
            Err(crate::interrupt::Interrupted.into())
        }
        res = cmd_future => res,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn account_override_is_global_and_redacted() {
        let cli = Cli::try_parse_from(["fj", "pr", "list", "--account", "qa-private"]).unwrap();
        assert!(cli.account.is_some());
        assert!(!format!("{cli:?}").contains("qa-private"));
    }
}

#[path = "invocation_profiles.rs"]
mod profiles;
