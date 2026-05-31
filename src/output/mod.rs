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

/// Truncate `s` to at most `max` characters, replacing the tail with a
/// single-character ellipsis when it overflows. Counts by char, not byte,
/// so multibyte content is clamped safely.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Format a byte count as a human-readable size (e.g. `512 B`, `1.0 KB`).
/// Uses 1024-based units up to TB.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_untouched() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string_gets_ellipsis() {
        let out = truncate("abcdefghij", 5);
        assert_eq!(out.chars().count(), 5);
        assert!(out.ends_with('…'));
        assert_eq!(out, "abcd…");
    }

    #[test]
    fn truncate_counts_chars_not_bytes() {
        // 6 multibyte chars, clamp to 4 → 3 chars + ellipsis.
        let out = truncate("αααααα", 4);
        assert_eq!(out.chars().count(), 4);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn human_size_formats_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0 GB");
        assert_eq!(human_size(2 * 1024_u64.pow(4)), "2.0 TB");
    }
}
