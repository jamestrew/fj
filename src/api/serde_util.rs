//! Small serde helpers for the Forgejo API's habit of returning `null` in
//! places where most clients expect an empty array.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Use as `#[serde(default, deserialize_with = "deserialize_null_to_default")]`
/// on fields that should map `null` to `Default::default()` (typically empty
/// `Vec` or empty `String`).
pub fn deserialize_null_to_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    let opt = Option::<T>::deserialize(d)?;
    Ok(opt.unwrap_or_default())
}

/// Use as `#[serde(default, deserialize_with = "string_or_number")]`.
/// Forgejo is inconsistent about some id fields: older Gitea-derived
/// builds return an integer row id, newer Forgejo returns the workflow
/// file path (e.g. `"ci.yml"`). Accept either (and `null`) and normalize
/// to `String` so the client never crashes on the variant.
pub fn string_or_number<'de, D>(d: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(d)? {
        Value::String(s) => Ok(s),
        Value::Number(n) => Ok(n.to_string()),
        Value::Null => Ok(String::new()),
        other => Err(serde::de::Error::custom(format!(
            "expected string or number, got {other}"
        ))),
    }
}
