use std::io::{self, Read};

use anyhow::{anyhow, Context, Result};
use clap::Args;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Method;
use serde_json::Value;
use url::Url;

use crate::client::{check_body_size, pagination::parse_link_header, Client};
use crate::output;
use crate::output::json_path::extract_path;

#[derive(Debug, Args)]
pub struct ApiArgs {
    /// API path, e.g. `/repos/owner/name` or `repos/owner/name`. Absolute URLs
    /// are also accepted.
    pub endpoint: String,

    /// HTTP method. Defaults to GET, or POST when `--input` is supplied.
    #[arg(short = 'X', long = "method")]
    pub method: Option<String>,

    /// Add a query parameter for GET/DELETE (`-f key=value`); for non-GET
    /// methods these are sent as JSON body fields. Repeatable.
    #[arg(short = 'f', long = "field", value_name = "KEY=VALUE")]
    pub fields: Vec<String>,

    /// Like `-f` but interprets the value as raw JSON.
    #[arg(short = 'F', long = "raw-field", value_name = "KEY=VALUE")]
    pub raw_fields: Vec<String>,

    /// Add a custom request header (`-H name=value`). Repeatable.
    #[arg(short = 'H', long = "header", value_name = "NAME=VALUE")]
    pub headers: Vec<String>,

    /// Output a JSON path projection. Supports dot paths, [N] indices,
    /// negative indices like [-1], and pipes (`. | .field`).
    #[arg(short = 'q', long = "jq")]
    pub jq: Option<String>,

    /// Send a JSON request body. Use `-` for stdin or `@file` for a file.
    /// Conflicts with `-f` / `-F`.
    #[arg(long, conflicts_with_all = ["fields", "raw_fields"])]
    pub input: Option<String>,

    /// Follow `Link: rel=next` and concatenate all pages into a single array.
    /// Only valid for GET requests whose body is an array.
    #[arg(long)]
    pub paginate: bool,

    /// Print response headers before the body.
    #[arg(short = 'i', long)]
    pub include: bool,

    /// Suppress response body output (still surfaces errors).
    #[arg(long)]
    pub silent: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(args: ApiArgs, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    let method = pick_method(&args)?;
    let (query, body) = build_query_or_body(&args, &method)?;
    let extra_headers = parse_headers(&args.headers)?;

    if args.paginate {
        if method != Method::GET {
            return Err(anyhow!("--paginate only works with GET"));
        }
        let pages = collect_paginated(&client, &args.endpoint, &query, &extra_headers).await?;
        let projected = match &args.jq {
            Some(path) => extract_path(&pages, path)?,
            None => pages,
        };
        if !args.silent {
            output::print_json(&projected)?;
        }
        return Ok(());
    }

    let res = client
        .request_with_headers(
            method.clone(),
            &args.endpoint,
            &query,
            body.as_ref(),
            &extra_headers,
        )
        .await?;
    let status = res.status();
    check_body_size(&res)?;
    let resp_headers = res.headers().clone();
    let text = res.text().await.context("reading response body")?;

    if args.include {
        eprintln!("HTTP {status}");
        for (k, v) in resp_headers.iter() {
            eprintln!("{}: {}", k.as_str(), v.to_str().unwrap_or("(binary)"));
        }
        eprintln!();
    }

    let parsed: Value = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text.clone()))
    };

    if !status.is_success() {
        eprintln!("HTTP {status}");
        eprintln!("{}", error_body_text(&parsed)?);
        return Err(anyhow!("request failed"));
    }

    let projected = match &args.jq {
        Some(path) => extract_path(&parsed, path)?,
        None => parsed,
    };
    if !args.silent {
        output::print_json(&projected)?;
    }
    Ok(())
}

fn error_body_text(value: &Value) -> Result<String> {
    output::json_text(value)
}

fn pick_method(args: &ApiArgs) -> Result<Method> {
    if let Some(m) = &args.method {
        return Method::from_bytes(m.to_uppercase().as_bytes())
            .with_context(|| format!("invalid HTTP method '{m}'"));
    }
    if args.input.is_some() {
        return Ok(Method::POST);
    }
    Ok(Method::GET)
}

type QueryAndBody = (Vec<(String, String)>, Option<Value>);

fn build_query_or_body(args: &ApiArgs, method: &Method) -> Result<QueryAndBody> {
    let mut stdin = io::stdin();
    build_query_or_body_with_reader(args, method, &mut stdin)
}

fn build_query_or_body_with_reader<R: Read>(
    args: &ApiArgs,
    method: &Method,
    input_reader: &mut R,
) -> Result<QueryAndBody> {
    if let Some(input) = &args.input {
        let body = read_input_body(input, input_reader)?;
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

fn read_input_body<R: Read>(input: &str, input_reader: &mut R) -> Result<Value> {
    let text = match input {
        "-" | "@-" => {
            let mut buf = String::new();
            input_reader
                .read_to_string(&mut buf)
                .context("reading --input from stdin")?;
            buf
        }
        path if path.starts_with('@') => {
            let path = &path[1..];
            std::fs::read_to_string(path).with_context(|| format!("reading --input file {path}"))?
        }
        literal => literal.to_string(),
    };
    serde_json::from_str(&text).context("--input must be valid JSON")
}

fn parse_headers(items: &[String]) -> Result<HeaderMap> {
    let mut map = HeaderMap::new();
    for item in items {
        let (k, v) = item
            .split_once(':')
            .or_else(|| item.split_once('='))
            .ok_or_else(|| anyhow!("expected NAME: VALUE, got '{item}'"))?;
        let name = HeaderName::from_bytes(k.trim().as_bytes())
            .with_context(|| format!("invalid header name '{k}'"))?;
        let value = HeaderValue::from_str(v.trim())
            .with_context(|| format!("invalid header value for '{k}'"))?;
        map.insert(name, value);
    }
    Ok(map)
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn collect_paginated(
    client: &Client,
    endpoint: &str,
    query: &[(String, String)],
    extra_headers: &HeaderMap,
) -> Result<Value> {
    // A hostile or buggy server can serve an endless `Link: rel=next` chain.
    // Cap the number of pages so `--paginate` can't be turned into an
    // unbounded loop / memory sink.
    const MAX_PAGES: usize = 1000;
    let mut all_items = Vec::<Value>::new();
    let mut current_endpoint: String = endpoint.to_string();
    let mut current_query: Vec<(String, String)> = query.to_vec();
    for page in 0.. {
        if page >= MAX_PAGES {
            return Err(anyhow!(
                "--paginate exceeded {MAX_PAGES} pages; aborting (server may be returning an endless `next` link)"
            ));
        }
        let res = client
            .request_with_headers(
                Method::GET,
                &current_endpoint,
                &current_query,
                None,
                extra_headers,
            )
            .await?;
        check_body_size(&res)?;
        let headers = res.headers().clone();
        let status = res.status();
        let text = res.text().await?;
        if !status.is_success() {
            return Err(anyhow!("HTTP {status}: {text}"));
        }
        let v: Value = serde_json::from_str(&text)?;
        match v {
            Value::Array(items) => all_items.extend(items),
            other => {
                if all_items.is_empty() {
                    return Ok(other);
                }
                return Err(anyhow!(
                    "expected an array body for --paginate, got: {other}"
                ));
            }
        }

        let next = headers
            .get(reqwest::header::LINK)
            .and_then(|l| l.to_str().ok())
            .map(parse_link_header)
            .and_then(|links| {
                links
                    .into_iter()
                    .find_map(|(url, rel)| (rel == "next").then_some(url))
            });
        let Some(next_url) = next else { break };

        // Forgejo's `next` is a full URL with the page query param updated.
        // Re-parse to extract the path + query so it threads through our
        // client (which adds auth + headers).
        let parsed = Url::parse(&next_url).context("parsing next-page URL")?;
        current_endpoint = parsed.path().to_string();
        current_query = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
    }
    Ok(Value::Array(all_items))
}

fn split_kv(s: &str) -> Result<(&str, &str)> {
    s.split_once('=')
        .ok_or_else(|| anyhow!("expected KEY=VALUE, got '{s}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_kv_works() {
        let (k, v) = split_kv("a=b").unwrap();
        assert_eq!(k, "a");
        assert_eq!(v, "b");
    }

    #[test]
    fn split_kv_keeps_equals_in_value() {
        let (k, v) = split_kv("filter=a=b").unwrap();
        assert_eq!(k, "filter");
        assert_eq!(v, "a=b");
    }

    #[test]
    fn parse_headers_basic() {
        let h = parse_headers(&["X-Custom: hello".into()]).unwrap();
        assert_eq!(h.get("x-custom").unwrap(), "hello");
    }

    #[test]
    fn parse_headers_equals_form() {
        let h = parse_headers(&["X-Custom=world".into()]).unwrap();
        assert_eq!(h.get("x-custom").unwrap(), "world");
    }

    #[test]
    fn parse_headers_rejects_unfielded() {
        assert!(parse_headers(&["nope".into()]).is_err());
    }

    fn base_args() -> ApiArgs {
        ApiArgs {
            endpoint: "/repos/o/n".into(),
            method: None,
            fields: Vec::new(),
            raw_fields: Vec::new(),
            headers: Vec::new(),
            jq: None,
            input: None,
            paginate: false,
            include: false,
            silent: false,
        }
    }

    #[test]
    fn pick_method_defaults_to_get() {
        assert_eq!(pick_method(&base_args()).unwrap(), Method::GET);
    }

    #[test]
    fn pick_method_defaults_to_post_with_input() {
        let mut a = base_args();
        a.input = Some("{}".into());
        assert_eq!(pick_method(&a).unwrap(), Method::POST);
    }

    #[test]
    fn pick_method_honors_explicit_method_case_insensitively() {
        let mut a = base_args();
        a.method = Some("patch".into());
        assert_eq!(pick_method(&a).unwrap(), Method::PATCH);
    }

    #[test]
    fn error_body_text_formats_json_for_stderr_without_stdout_printing() {
        let text = error_body_text(&serde_json::json!({ "message": "not found" })).unwrap();
        assert!(text.contains("not found"));
        assert!(!text.ends_with("\n"));
    }

    #[test]
    fn pick_method_rejects_garbage() {
        let mut a = base_args();
        a.method = Some("not a method".into());
        assert!(pick_method(&a).is_err());
    }

    #[test]
    fn build_query_or_body_puts_fields_in_query_for_get() {
        let mut a = base_args();
        a.fields = vec!["state=open".into()];
        a.raw_fields = vec!["limit=5".into()];
        let (query, body) = build_query_or_body(&a, &Method::GET).unwrap();
        assert!(body.is_none());
        assert_eq!(
            query,
            vec![
                ("state".to_string(), "open".to_string()),
                ("limit".to_string(), "5".to_string()),
            ]
        );
    }

    #[test]
    fn build_query_or_body_builds_json_body_for_post() {
        let mut a = base_args();
        a.fields = vec!["title=hi".into()];
        a.raw_fields = vec!["draft=true".into()];
        let (query, body) = build_query_or_body(&a, &Method::POST).unwrap();
        assert!(query.is_empty());
        let body = body.unwrap();
        // Plain fields stay strings; raw fields are parsed as JSON.
        assert_eq!(body["title"], "hi");
        assert_eq!(body["draft"], true);
    }

    #[test]
    fn build_query_or_body_rejects_invalid_raw_field_json() {
        let mut a = base_args();
        a.raw_fields = vec!["draft=not-json".into()];
        assert!(build_query_or_body(&a, &Method::POST).is_err());
    }

    #[test]
    fn build_query_or_body_input_overrides_fields() {
        let mut a = base_args();
        a.input = Some(r#"{"a":1}"#.into());
        let (query, body) = build_query_or_body(&a, &Method::POST).unwrap();
        assert!(query.is_empty());
        assert_eq!(body.unwrap()["a"], 1);
    }

    #[test]
    fn build_query_or_body_input_reads_stdin() {
        let mut a = base_args();
        a.input = Some("-".into());
        let mut input = std::io::Cursor::new(r#"{"a":2}"#);
        let (query, body) = build_query_or_body_with_reader(&a, &Method::POST, &mut input).unwrap();
        assert!(query.is_empty());
        assert_eq!(body.unwrap()["a"], 2);
    }

    #[test]
    fn build_query_or_body_input_reads_at_stdin() {
        let mut a = base_args();
        a.input = Some("@-".into());
        let mut input = std::io::Cursor::new(r#"{"a":3}"#);
        let (query, body) = build_query_or_body_with_reader(&a, &Method::POST, &mut input).unwrap();
        assert!(query.is_empty());
        assert_eq!(body.unwrap()["a"], 3);
    }

    #[test]
    fn build_query_or_body_input_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body.json");
        std::fs::write(&path, r#"{"a":4}"#).unwrap();
        let mut a = base_args();
        a.input = Some(format!("@{}", path.display()));
        let mut input = std::io::Cursor::new("");
        let (query, body) = build_query_or_body_with_reader(&a, &Method::POST, &mut input).unwrap();
        assert!(query.is_empty());
        assert_eq!(body.unwrap()["a"], 4);
    }

    #[test]
    fn build_query_or_body_input_keeps_bare_file_path_literal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body.json");
        std::fs::write(&path, r#"{"a":5}"#).unwrap();
        let mut a = base_args();
        a.input = Some(path.display().to_string());
        let mut input = std::io::Cursor::new("");
        let err = build_query_or_body_with_reader(&a, &Method::POST, &mut input).unwrap_err();
        assert!(format!("{err:#}").contains("--input must be valid JSON"));
    }

    #[test]
    fn build_query_or_body_rejects_invalid_stdin_input_json() {
        let mut a = base_args();
        a.input = Some("-".into());
        let mut input = std::io::Cursor::new("{bad");
        let err = build_query_or_body_with_reader(&a, &Method::POST, &mut input).unwrap_err();
        assert!(format!("{err:#}").contains("--input must be valid JSON"));
    }

    #[test]
    fn build_query_or_body_rejects_invalid_input_json() {
        let mut a = base_args();
        a.input = Some("{bad".into());
        assert!(build_query_or_body(&a, &Method::POST).is_err());
    }
}
