use anyhow::{anyhow, Context, Result};
use clap::Args;
use reqwest::Method;
use serde_json::Value;

use crate::client::Client;
use crate::output;

#[derive(Debug, Args)]
pub struct ApiArgs {
    /// API path, e.g. `/repos/owner/name` or `repos/owner/name`. Absolute URLs
    /// are also accepted.
    pub endpoint: String,

    /// HTTP method. Defaults to GET, or POST when fields are supplied.
    #[arg(short = 'X', long = "method")]
    pub method: Option<String>,

    /// Add a query parameter (`-f key=value`). For GET; for non-GET methods these
    /// are sent as a JSON body field instead. Repeatable.
    #[arg(short = 'f', long = "field", value_name = "KEY=VALUE")]
    pub fields: Vec<String>,

    /// Like `-f` but interprets the value as raw JSON (numbers, bools, arrays, objects).
    #[arg(short = 'F', long = "raw-field", value_name = "KEY=VALUE")]
    pub raw_fields: Vec<String>,

    /// Output a specific JSON path (very small jq-ish subset, e.g. `.items.0.title`).
    #[arg(short = 'q', long = "jq")]
    pub jq: Option<String>,

    /// Send a literal JSON request body. Conflicts with `-f` / `-F`.
    #[arg(long, conflicts_with_all = ["fields", "raw_fields"])]
    pub input: Option<String>,
}

pub async fn run(args: ApiArgs, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;

    let method = pick_method(&args)?;
    let (query, body) = build_query_or_body(&args, &method)?;

    let res = client
        .request(method.clone(), &args.endpoint, &query, body.as_ref())
        .await?;
    let status = res.status();
    let text = res.text().await.context("reading response body")?;

    let parsed: Value = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.clone()))
    };

    if !status.is_success() {
        eprintln!("HTTP {status}");
        output::print_json(&parsed)?;
        return Err(anyhow!("request failed"));
    }

    let projected = match &args.jq {
        Some(path) => extract_path(&parsed, path)?,
        None => parsed,
    };
    output::print_json(&projected)
}

fn pick_method(args: &ApiArgs) -> Result<Method> {
    if let Some(m) = &args.method {
        return Method::from_bytes(m.to_uppercase().as_bytes())
            .with_context(|| format!("invalid HTTP method '{m}'"));
    }
    // Only `--input` (a literal JSON body) implies POST. `-f` / `-F` on a GET
    // are sent as query params, which is what users hit search endpoints for.
    // To send a write request, pass `-X POST` (or PATCH/PUT/DELETE) explicitly.
    if args.input.is_some() {
        return Ok(Method::POST);
    }
    Ok(Method::GET)
}

fn build_query_or_body(
    args: &ApiArgs,
    method: &Method,
) -> Result<(Vec<(String, String)>, Option<Value>)> {
    if let Some(input) = &args.input {
        let body: Value =
            serde_json::from_str(input).context("--input must be valid JSON")?;
        return Ok((Vec::new(), Some(body)));
    }

    if method == Method::GET || method == Method::DELETE {
        let mut q = Vec::new();
        for f in &args.fields {
            let (k, v) = split_kv(f)?;
            q.push((k.to_string(), v.to_string()));
        }
        for f in &args.raw_fields {
            let (k, v) = split_kv(f)?;
            q.push((k.to_string(), v.to_string()));
        }
        return Ok((q, None));
    }

    let mut obj = serde_json::Map::new();
    for f in &args.fields {
        let (k, v) = split_kv(f)?;
        obj.insert(k.to_string(), Value::String(v.to_string()));
    }
    for f in &args.raw_fields {
        let (k, v) = split_kv(f)?;
        let parsed: Value = serde_json::from_str(v)
            .with_context(|| format!("--raw-field {k}: value '{v}' is not valid JSON"))?;
        obj.insert(k.to_string(), parsed);
    }
    Ok((Vec::new(), Some(Value::Object(obj))))
}

fn split_kv(s: &str) -> Result<(&str, &str)> {
    s.split_once('=')
        .ok_or_else(|| anyhow!("expected KEY=VALUE, got '{s}'"))
}

/// Very small jq-ish projector: dot-separated keys + numeric indices.
/// e.g. `.data.0.name`, `.message`.
fn extract_path(value: &Value, path: &str) -> Result<Value> {
    let path = path.trim_start_matches('.');
    if path.is_empty() {
        return Ok(value.clone());
    }
    let mut current = value;
    for segment in path.split('.') {
        if let Ok(idx) = segment.parse::<usize>() {
            current = current
                .get(idx)
                .ok_or_else(|| anyhow!("index {idx} out of range at '{segment}'"))?;
        } else {
            current = current
                .get(segment)
                .ok_or_else(|| anyhow!("no field '{segment}'"))?;
        }
    }
    Ok(current.clone())
}
