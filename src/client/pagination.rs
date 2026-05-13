use reqwest::header::HeaderMap;

/// A single page of items along with parsed pagination headers. Forgejo
/// follows Gitea's convention: `Link` header in RFC 5988 style plus
/// `X-Total-Count`.
pub struct Page<T> {
    pub items: Vec<T>,
    #[allow(dead_code)]
    pub next: Option<String>,
    #[allow(dead_code)]
    pub prev: Option<String>,
    #[allow(dead_code)]
    pub last: Option<String>,
    #[allow(dead_code)]
    pub first: Option<String>,
    #[allow(dead_code)]
    pub total: Option<u64>,
}

impl<T> Page<T> {
    pub fn from_headers(items: Vec<T>, headers: &HeaderMap) -> Self {
        let mut next = None;
        let mut prev = None;
        let mut last = None;
        let mut first = None;
        if let Some(link) = headers.get(reqwest::header::LINK) {
            if let Ok(link_str) = link.to_str() {
                for (url, rel) in parse_link_header(link_str) {
                    match rel.as_str() {
                        "next" => next = Some(url),
                        "prev" => prev = Some(url),
                        "last" => last = Some(url),
                        "first" => first = Some(url),
                        _ => {}
                    }
                }
            }
        }
        let total = headers
            .get("x-total-count")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        Self {
            items,
            next,
            prev,
            last,
            first,
            total,
        }
    }
}

pub(crate) fn parse_link_header(value: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in value.split(',') {
        let part = part.trim();
        // `<url>; rel="name"`
        let Some((url_part, params)) = part.split_once(';') else {
            continue;
        };
        let url = url_part
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>');
        let rel = params.split(';').map(str::trim).find_map(|p| {
            let (k, v) = p.split_once('=')?;
            if k.trim().eq_ignore_ascii_case("rel") {
                Some(v.trim().trim_matches('"').to_string())
            } else {
                None
            }
        });
        if let Some(rel) = rel {
            out.push((url.to_string(), rel));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_link() {
        let input = r#"<https://example.com/api/v1/repos?page=2>; rel="next""#;
        let parsed = parse_link_header(input);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "https://example.com/api/v1/repos?page=2");
        assert_eq!(parsed[0].1, "next");
    }

    #[test]
    fn parses_multiple_rels() {
        let input = r#"<https://x/p?page=2>; rel="next", <https://x/p?page=5>; rel="last", <https://x/p?page=1>; rel="first""#;
        let parsed = parse_link_header(input);
        let rels: Vec<&str> = parsed.iter().map(|(_, r)| r.as_str()).collect();
        assert!(rels.contains(&"next"));
        assert!(rels.contains(&"last"));
        assert!(rels.contains(&"first"));
    }

    #[test]
    fn ignores_unparsable_segments() {
        let input = "garbage";
        assert!(parse_link_header(input).is_empty());
    }

    #[test]
    fn rel_case_insensitive() {
        let input = r#"<https://x/p>; REL="next""#;
        let parsed = parse_link_header(input);
        assert_eq!(parsed[0].1, "next");
    }
}
