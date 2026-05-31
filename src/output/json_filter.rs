//! gh-style `--json field1,field2` projection. Given a JSON value (which is
//! typically either an object representing a single resource or an array of
//! such objects) and a comma-separated list of field names, return a
//! projection that contains only those fields.
//!
//! Nested fields (`a.b`) are supported. Missing fields produce `null` rather
//! than an error, matching gh's behavior.

use serde_json::{Map, Value};

/// Parse `"a,b , c"` into `["a", "b", "c"]`.
pub fn parse_fields(spec: &str) -> Vec<String> {
    spec.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Project a value down to the requested fields. If `value` is an array,
/// each element is projected independently. Other scalar values pass through
/// unchanged.
pub fn project(value: &Value, fields: &[String]) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(|v| project_one(v, fields)).collect()),
        Value::Object(_) => project_one(value, fields),
        other => other.clone(),
    }
}

fn project_one(value: &Value, fields: &[String]) -> Value {
    let mut out = Map::new();
    for f in fields {
        // The output key is the full dotted path so users can disambiguate.
        // A user asking for `owner.login` gets that exact key in the result.
        // We share the json_path engine but, gh-style, a missing field
        // resolves to null instead of erroring.
        let extracted = super::json_path::extract_path(value, f).unwrap_or(Value::Null);
        out.insert(f.clone(), extracted);
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_field_list() {
        assert_eq!(parse_fields("a,b,c"), vec!["a", "b", "c"]);
        assert_eq!(parse_fields(" a , b , c "), vec!["a", "b", "c"]);
        assert_eq!(parse_fields(""), Vec::<String>::new());
        assert_eq!(parse_fields(", ,a"), vec!["a"]);
    }

    #[test]
    fn projects_single_object() {
        let v = json!({"id": 1, "title": "hi", "body": "long"});
        let p = project(&v, &["id".into(), "title".into()]);
        assert_eq!(p, json!({"id": 1, "title": "hi"}));
    }

    #[test]
    fn projects_array_of_objects() {
        let v = json!([
            {"id": 1, "title": "a", "body": "x"},
            {"id": 2, "title": "b", "body": "y"},
        ]);
        let p = project(&v, &["id".into(), "title".into()]);
        assert_eq!(
            p,
            json!([
                {"id": 1, "title": "a"},
                {"id": 2, "title": "b"},
            ])
        );
    }

    #[test]
    fn supports_nested_fields() {
        let v = json!({"owner": {"login": "alice", "id": 7}});
        let p = project(&v, &["owner.login".into()]);
        assert_eq!(p, json!({"owner.login": "alice"}));
    }

    #[test]
    fn missing_field_yields_null() {
        let v = json!({"id": 1});
        let p = project(&v, &["id".into(), "missing".into()]);
        assert_eq!(p, json!({"id": 1, "missing": null}));
    }

    #[test]
    fn scalar_passes_through() {
        let v = json!("hello");
        assert_eq!(project(&v, &["id".into()]), json!("hello"));
    }

    #[test]
    fn missing_nested_field_still_yields_null() {
        // A miss anywhere along the path resolves to null, not an error,
        // even though the shared engine would error on the bare path.
        let v = json!({"owner": {"id": 7}});
        let p = project(&v, &["owner.login".into()]);
        assert_eq!(p, json!({"owner.login": null}));
    }

    #[test]
    fn array_index_paths_work_via_the_shared_engine() {
        // Sharing the json_path engine means --json-fields now also accepts
        // the richer path syntax (indices/brackets) as a superset.
        let v = json!({"labels": [{"name": "bug"}, {"name": "docs"}]});
        let p = project(&v, &["labels.0.name".into(), "labels[1].name".into()]);
        assert_eq!(p, json!({"labels.0.name": "bug", "labels[1].name": "docs"}));
    }
}
