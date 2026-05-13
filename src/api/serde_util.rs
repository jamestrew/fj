//! Small serde helpers for the Forgejo API's habit of returning `null` in
//! places where most clients expect an empty array.

use serde::{Deserialize, Deserializer};

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
