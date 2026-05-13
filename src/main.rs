mod api;
mod auth;
mod cli;
mod client;
mod config;
mod git;
mod output;

use std::process::ExitCode;

use clap::Parser;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = cli::Cli::parse();

    if let Err(err) = cli::run(cli).await {
        // Print the full chain of causes, matching the gh-style error output.
        eprintln!("{} {err}", owo_colors::OwoColorize::red(&"error:"));
        let mut source = err.source();
        while let Some(cause) = source {
            eprintln!("  caused by: {cause}");
            source = cause.source();
        }
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
