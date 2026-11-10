//! Alternate identity carriers must not override the verified invocation PAT.
use super::*;

fn credential_name(name: &str) -> bool {
    ["token", "access_token", "sudo"]
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

pub(super) fn check(
    client: &Client,
    path: &str,
    query: &[(String, String)],
    headers: &HeaderMap,
) -> Result<()> {
    let url = client
        .url(path)
        .map_err(|_| anyhow!("invalid account override request URL"))?;
    // ParseForm also reads URL-encoded bodies, before the Authorization header.
    let form_body = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/x-www-form-urlencoded")
        });
    if form_body
        || url.query_pairs().any(|(name, _)| credential_name(&name))
        || query.iter().any(|(name, _)| credential_name(name))
        || ["authorization", proxy::FORWARDED_AUTH_HEADER, "sudo"]
            .iter()
            .any(|name| headers.contains_key(*name))
    {
        return Err(anyhow!(
            "alternate authentication conflicts with account override"
        ));
    }
    Ok(())
}
