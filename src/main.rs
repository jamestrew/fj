// `coverage_nightly` is set by cargo-llvm-cov on a nightly toolchain. Gating
// the unstable `coverage_attribute` feature on it keeps stable builds (the
// MSRV path) completely unaffected, while `make coverage-strict` can drop the
// async command handlers (which we don't unit-test) from the report. See the
// Makefile's COV_IGNORE section.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod api;
mod auth;
mod cli;
mod client;
mod config;
mod fjord;
mod git;
mod output;

use std::process::{Command, ExitCode};

use reqwest::StatusCode;

use clap::Parser;

use crate::output::logo;

#[tokio::main]
async fn main() -> ExitCode {
    // Two pre-parse transformations, in order:
    //   1. expand a user-defined alias for `fj <name>`
    //   2. dispatch to a `fj-<name>` plugin binary on PATH if clap can't
    //      recognize the subcommand (matches gh's convention)
    let argv: Vec<String> = std::env::args().collect();
    let argv = cli::alias::expand_argv(argv);

    if let Some(plugin) = detect_plugin(&argv) {
        let prog = format!("fj-{plugin}");
        let extra: Vec<&String> = argv.iter().skip(2).collect();
        let status = match Command::new(&prog).args(&extra).status() {
            Ok(s) => s,
            Err(_) => {
                // Plugin not found — fall through to clap, which will error
                // nicely.
                return run_cli(argv).await;
            }
        };
        return ExitCode::from(status.code().unwrap_or(1) as u8);
    }

    run_cli(argv).await
}

async fn run_cli(argv: Vec<String>) -> ExitCode {
    let cli = match cli::Cli::try_parse_from(argv) {
        Ok(cli) => cli,
        Err(err) => return handle_parse_outcome(err),
    };
    if let Err(err) = cli::run(cli).await {
        eprintln!("{} {err}", owo_colors::OwoColorize::red(&"error:"));
        if client::debug_enabled() {
            let mut source = err.source();
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
        }
        return ExitCode::from(error_exit_code(&err));
    }
    ExitCode::SUCCESS
}

const EXIT_GENERIC: u8 = 1;
const EXIT_NOT_FOUND: u8 = 3;
const EXIT_AUTH_FORBIDDEN: u8 = 4;
const EXIT_RATE_LIMIT_TRANSIENT: u8 = 5;

fn error_exit_code(err: &anyhow::Error) -> u8 {
    let api = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<client::ApiError>());
    match api.map(|err| err.status) {
        Some(StatusCode::NOT_FOUND) => EXIT_NOT_FOUND,
        Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => EXIT_AUTH_FORBIDDEN,
        Some(StatusCode::TOO_MANY_REQUESTS) => EXIT_RATE_LIMIT_TRANSIENT,
        Some(status) if status.is_server_error() => EXIT_RATE_LIMIT_TRANSIENT,
        _ => EXIT_GENERIC,
    }
}

/// clap surfaces `--help`, `--version`, and the bare-`fj` help screen as
/// `Err(..)` from `try_parse_from`. For those three (and only those), greet
/// with the brand mark first, but only on an interactive stdout so piped
/// `fj --help` / `fj --version` stay clean and parseable. Everything else is a
/// real usage error: print it as clap formatted it and exit with its code.
fn handle_parse_outcome(err: clap::Error) -> ExitCode {
    use clap::error::ErrorKind;
    use std::io::IsTerminal;

    let show_logo = match err.kind() {
        // The version line, explicit help screen, and bare-`fj` help screen
        // only get the mark on an interactive stdout, so piped output stays
        // clean and parseable.
        ErrorKind::DisplayVersion
        | ErrorKind::DisplayHelp
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => std::io::stdout().is_terminal(),
        _ => false,
    };
    if show_logo {
        print!("{}", logo::banner());
    }
    let _ = err.print();
    ExitCode::from(err.exit_code() as u8)
}

/// Returns `Some(name)` if `argv[1]` is unrecognized by clap AND a `fj-<name>`
/// binary exists on PATH.
fn detect_plugin(argv: &[String]) -> Option<String> {
    if argv.len() < 2 {
        return None;
    }
    let candidate = &argv[1];
    // Skip if it's clap-internal or obviously a flag.
    if candidate.starts_with('-') {
        return None;
    }
    if KNOWN_SUBCOMMANDS.iter().any(|s| s == candidate) {
        return None;
    }
    // Look up `fj-<name>` on PATH.
    let bin = format!("fj-{candidate}");
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let full = dir.join(&bin);
        if full.is_file() {
            return Some(candidate.to_string());
        }
    }
    None
}

/// Keep this list in sync with `cli::Command`. It's an allowlist used to avoid
/// hijacking real subcommands as plugin candidates.
const KNOWN_SUBCOMMANDS: &[&str] = &[
    "auth",
    "instances",
    "repo",
    "issue",
    "pr",
    "release",
    "label",
    "milestone",
    "run",
    "workflow",
    "secret",
    "variable",
    "search",
    "browse",
    "status",
    "org",
    "ssh-key",
    "gpg-key",
    "alias",
    "config",
    "extension",
    "protect",
    "hook",
    "api",
    "work",
    "stack",
    "agent",
    "completion",
    "man",
    "help",
];
