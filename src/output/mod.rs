pub mod json_filter;
pub mod pager;

use std::io::IsTerminal;
use std::sync::OnceLock;

use anyhow::Result;

static JSON_FIELDS: OnceLock<Vec<String>> = OnceLock::new();

/// Set the global `--json-fields` selector. Called once at startup from
/// `cli::run`. After this, every `print_json` call routes through the filter.
pub fn set_json_fields(spec: Option<&str>) {
    let parsed: Vec<String> = match spec {
        Some(s) => json_filter::parse_fields(s),
        None => Vec::new(),
    };
    let _ = JSON_FIELDS.set(parsed);
}

fn json_fields() -> Option<&'static [String]> {
    JSON_FIELDS
        .get()
        .filter(|v| !v.is_empty())
        .map(|v| v.as_slice())
}
use owo_colors::{OwoColorize, Style};
use tabled::settings::{object::Columns, Alignment, Modify, Padding, Style as TStyle};
use tabled::{builder::Builder, Table};

#[allow(dead_code)]
pub fn stdout_is_tty() -> bool {
    std::io::stdout().is_terminal()
}

pub fn supports_color() -> bool {
    supports_color::on(supports_color::Stream::Stdout).is_some()
}

/// Pretty-print a list as a borderless gh-style table. Header row is
/// rendered bold when color is supported.
pub fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut builder = Builder::default();
    let header_row: Vec<String> = if supports_color() {
        headers
            .iter()
            .map(|h| h.style(Style::new().bold()).to_string())
            .collect()
    } else {
        headers.iter().map(|h| h.to_string()).collect()
    };
    builder.push_record(header_row);
    for row in rows {
        builder.push_record(row.clone());
    }
    let mut table: Table = builder.build();
    table
        .with(TStyle::empty())
        .with(Padding::new(0, 2, 0, 0))
        .with(Modify::new(Columns::new(0..)).with(Alignment::left()));
    table.to_string()
}

/// Print JSON pretty-formatted to stdout. If `--json-fields foo,bar` was
/// set globally, the value is projected through that filter first.
pub fn print_json(value: &serde_json::Value) -> Result<()> {
    let projected = match json_fields() {
        Some(fs) => json_filter::project(value, fs),
        None => value.clone(),
    };
    let text = serde_json::to_string_pretty(&projected)?;
    println!("{text}");
    Ok(())
}

/// gh-style state pill: green for open, magenta for merged, red for closed.
/// Always returns the capitalized label; color is applied only when the
/// terminal supports it. `merged=true` overrides `state` (which is `closed`
/// for merged PRs on the Forgejo API).
pub fn state_pill(state: &str, merged: bool) -> String {
    let (label, style) = match (state, merged) {
        (_, true) => ("Merged", Style::new().magenta().bold()),
        ("open", _) => ("Open", Style::new().green().bold()),
        ("closed", _) => ("Closed", Style::new().red().bold()),
        (other, _) => return other.to_string(),
    };
    if supports_color() {
        label.style(style).to_string()
    } else {
        label.to_string()
    }
}

/// Colorize an Actions status/conclusion word (`success`, `failure`,
/// `running`, …) for the terminal. Unknown words and no-color terminals get
/// the text back unchanged.
pub fn action_status(status: &str) -> String {
    if !supports_color() {
        return status.to_string();
    }
    let style = match status {
        "success" => Style::new().green().bold(),
        "failure" | "error" => Style::new().red().bold(),
        "cancelled" | "skipped" => Style::new().yellow(),
        "running" | "waiting" | "blocked" => Style::new().cyan(),
        _ => return status.to_string(),
    };
    status.style(style).to_string()
}

pub fn dim(s: &str) -> String {
    if supports_color() {
        s.dimmed().to_string()
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    if supports_color() {
        s.bold().to_string()
    } else {
        s.to_string()
    }
}

/// Approximate "2h ago" / "3d ago" rendering. Forgejo timestamps are UTC.
pub fn relative_time(t: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let delta = now.signed_duration_since(t);
    let secs = delta.num_seconds();
    let abs = secs.abs();
    let label = if abs < 60 {
        format!("{abs}s")
    } else if abs < 3600 {
        format!("{}m", abs / 60)
    } else if abs < 86_400 {
        format!("{}h", abs / 3600)
    } else if abs < 86_400 * 30 {
        format!("{}d", abs / 86_400)
    } else if abs < 86_400 * 365 {
        format!("{}mo", abs / (86_400 * 30))
    } else {
        format!("{}y", abs / (86_400 * 365))
    };
    if secs >= 0 {
        format!("{label} ago")
    } else {
        format!("in {label}")
    }
}
