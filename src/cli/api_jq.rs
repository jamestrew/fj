//! The small jq-ish path projector behind `fj api --jq`.
//!
//! Split out of `cli::api` to keep the request-building logic and the
//! projection logic in separate files. `extract_path` is the only entry
//! point; `apply_stage` / `split_segments` are private helpers.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

/// jq-ish projector. Supports:
/// - dot fields (`.a.b`)
/// - numeric indices (`.0`, `.data.1`)
/// - bracket indices (`.data[0]`, negative supported: `.data[-1]`)
/// - pipes (`.foo | .bar`)
pub(crate) fn extract_path(value: &Value, expression: &str) -> Result<Value> {
    let mut current = value.clone();
    for stage in expression.split('|') {
        current = apply_stage(&current, stage.trim())?;
    }
    Ok(current)
}

fn apply_stage(value: &Value, stage: &str) -> Result<Value> {
    let stage = stage.trim_start_matches('.');
    if stage.is_empty() {
        return Ok(value.clone());
    }

    let mut current = value;
    let mut owned: Option<Value> = None;

    for raw in split_segments(stage) {
        let target = owned.as_ref().unwrap_or(current);
        let raw_str = raw.as_str();
        if let Some(rest) = raw_str.strip_prefix('[') {
            let inner = rest
                .strip_suffix(']')
                .ok_or_else(|| anyhow!("unterminated bracket in '{raw_str}'"))?;
            let idx: i64 = inner
                .parse()
                .with_context(|| format!("invalid bracket index '{inner}'"))?;
            let Some(arr) = target.as_array() else {
                return Err(anyhow!("not an array; can't index with '{raw_str}'"));
            };
            let real = if idx < 0 { arr.len() as i64 + idx } else { idx };
            let v = arr
                .get(real as usize)
                .ok_or_else(|| anyhow!("index {idx} out of range"))?
                .clone();
            owned = Some(v);
            current = owned.as_ref().unwrap();
        } else if let Ok(idx) = raw_str.parse::<usize>() {
            let v = target
                .get(idx)
                .ok_or_else(|| anyhow!("index {idx} out of range"))?
                .clone();
            owned = Some(v);
            current = owned.as_ref().unwrap();
        } else {
            let v = target
                .get(raw_str)
                .ok_or_else(|| anyhow!("no field '{raw_str}'"))?
                .clone();
            owned = Some(v);
            current = owned.as_ref().unwrap();
        }
    }
    Ok(owned.unwrap_or_else(|| value.clone()))
}

/// Split a stage like `data.0[3].name` into ["data", "0", "[3]", "name"].
fn split_segments(stage: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut chars = stage.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if !buf.is_empty() {
                    out.push(std::mem::take(&mut buf));
                }
            }
            '[' => {
                if !buf.is_empty() {
                    out.push(std::mem::take(&mut buf));
                }
                let mut br = String::from('[');
                for c2 in chars.by_ref() {
                    br.push(c2);
                    if c2 == ']' {
                        break;
                    }
                }
                out.push(br);
            }
            _ => buf.push(c),
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_root() {
        let v = json!({"a": 1});
        assert_eq!(extract_path(&v, "").unwrap(), v);
        assert_eq!(extract_path(&v, ".").unwrap(), v);
    }

    #[test]
    fn extract_field() {
        let v = json!({"a": {"b": 42}});
        assert_eq!(extract_path(&v, ".a.b").unwrap(), json!(42));
    }

    #[test]
    fn extract_array_index_dotted() {
        let v = json!({"data": [{"name": "first"}, {"name": "second"}]});
        assert_eq!(extract_path(&v, ".data.1.name").unwrap(), json!("second"));
    }

    #[test]
    fn extract_array_index_bracket() {
        let v = json!({"data": [10, 20, 30]});
        assert_eq!(extract_path(&v, ".data[1]").unwrap(), json!(20));
    }

    #[test]
    fn extract_negative_index() {
        let v = json!([1, 2, 3, 4]);
        assert_eq!(extract_path(&v, "[-1]").unwrap(), json!(4));
        assert_eq!(extract_path(&v, "[-2]").unwrap(), json!(3));
    }

    #[test]
    fn extract_pipe() {
        let v = json!({"data": [{"x": 7}]});
        assert_eq!(extract_path(&v, ".data[0] | .x").unwrap(), json!(7));
    }

    #[test]
    fn extract_missing_field_errors() {
        let v = json!({"a": 1});
        assert!(extract_path(&v, ".missing").is_err());
    }

    #[test]
    fn extract_out_of_range_errors() {
        let v = json!([1, 2]);
        assert!(extract_path(&v, ".5").is_err());
        assert!(extract_path(&v, "[5]").is_err());
    }
}
