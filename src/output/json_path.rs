//! The one jq-ish JSON path engine, shared by both projectors:
//! - `fj api --jq` (via `cli::api`) wants the `Result` directly, so a bad
//!   path surfaces as an error.
//! - `--json-fields` (via `output::json_filter`) maps a miss to `null`,
//!   matching gh.
//!
//! Lives under `output` so both `cli` and `output` can depend on it without
//! inverting the module layering. Supports:
//! - dot fields (`.a.b`)
//! - numeric indices (`.0`, `.data.1`)
//! - bracket indices (`.data[0]`, negative supported: `.data[-1]`)
//! - array iterate (`.[]`, `.data[].name`): maps the rest of the path over
//!   every element of an array (or every value of an object) and collects the
//!   results into a JSON array
//! - pipes (`.foo | .bar`)

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

pub fn extract_path(value: &Value, expression: &str) -> Result<Value> {
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
    apply_segments(value, &split_segments(stage))
}

/// Walk `segments` against `value`. An empty-bracket segment (`[]`) fans out:
/// the remaining segments are applied to each element and collected into an
/// array, so `.[].title` over a list of objects yields a list of titles.
fn apply_segments(value: &Value, segments: &[String]) -> Result<Value> {
    let Some((raw, rest)) = segments.split_first() else {
        return Ok(value.clone());
    };
    let raw_str = raw.as_str();

    if raw_str == "[]" {
        let items: Vec<&Value> = match value {
            Value::Array(a) => a.iter().collect(),
            Value::Object(o) => o.values().collect(),
            _ => return Err(anyhow!("can't iterate with '[]' over a non-array/object")),
        };
        let mapped = items
            .into_iter()
            .map(|el| apply_segments(el, rest))
            .collect::<Result<Vec<_>>>()?;
        return Ok(Value::Array(mapped));
    }

    if let Some(inner) = raw_str.strip_prefix('[') {
        let inner = inner
            .strip_suffix(']')
            .ok_or_else(|| anyhow!("unterminated bracket in '{raw_str}'"))?;
        let idx: i64 = inner
            .parse()
            .with_context(|| format!("invalid bracket index '{inner}'"))?;
        let Some(arr) = value.as_array() else {
            return Err(anyhow!("not an array; can't index with '{raw_str}'"));
        };
        let real = if idx < 0 { arr.len() as i64 + idx } else { idx };
        let next = arr
            .get(real as usize)
            .ok_or_else(|| anyhow!("index {idx} out of range"))?;
        return apply_segments(next, rest);
    }

    if let Ok(idx) = raw_str.parse::<usize>() {
        let next = value
            .get(idx)
            .ok_or_else(|| anyhow!("index {idx} out of range"))?;
        return apply_segments(next, rest);
    }

    let next = value
        .get(raw_str)
        .ok_or_else(|| anyhow!("no field '{raw_str}'"))?;
    apply_segments(next, rest)
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
    fn extract_iterate_field() {
        let v = json!([{"title": "a"}, {"title": "b"}, {"title": "c"}]);
        assert_eq!(
            extract_path(&v, ".[].title").unwrap(),
            json!(["a", "b", "c"])
        );
        // leading dot is optional, matching the rest of the engine
        assert_eq!(
            extract_path(&v, "[].title").unwrap(),
            json!(["a", "b", "c"])
        );
    }

    #[test]
    fn extract_iterate_nested() {
        let v = json!({"data": [{"owner": {"login": "alice"}}, {"owner": {"login": "bob"}}]});
        assert_eq!(
            extract_path(&v, ".data[].owner.login").unwrap(),
            json!(["alice", "bob"])
        );
    }

    #[test]
    fn extract_iterate_bare_returns_elements() {
        let arr = json!([1, 2, 3]);
        assert_eq!(extract_path(&arr, ".[]").unwrap(), json!([1, 2, 3]));
        // over an object, iterate yields its values (insertion order)
        let obj = json!({"a": 1, "b": 2});
        assert_eq!(extract_path(&obj, ".[]").unwrap(), json!([1, 2]));
    }

    #[test]
    fn extract_iterate_over_scalar_errors() {
        let v = json!({"n": 5});
        assert!(extract_path(&v, ".n[]").is_err());
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
