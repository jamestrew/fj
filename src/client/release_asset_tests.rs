use super::*;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const HTML: &str = "https://forge.example/o/r/releases/tag/v1";
const ASSET: &str = "https://forge.example/o/r/releases/download/v1/SHA256SUMS";

fn fjord(platform: &str) -> Client {
    let base = Url::parse(&format!("{platform}/api/v1/forge-gateway/inst_x/api/v1/")).unwrap();
    Client::for_base_url_with_scheme(base, Some("jwt".into()), AuthScheme::FjordBearer).unwrap()
}

#[test]
fn asset_maps_to_the_gateway_path_of_the_same_release() {
    let url = fjord("https://platform.example")
        .gateway_release_asset(("o", "r"), HTML, ASSET, "v1", "SHA256SUMS")
        .unwrap()
        .unwrap();
    assert_eq!(
        url.as_str(),
        "https://platform.example/api/v1/forge-gateway/inst_x/o/r/releases/download/v1/SHA256SUMS"
    );
}

#[test]
fn a_path_mounted_instance_or_another_repository_is_refused() {
    let c = fjord("https://platform.example");
    let mounted = (
        "https://forge.example/base/o/r/releases/tag/v1",
        "https://forge.example/base/o/r/releases/download/v1/a.zip",
    );
    assert!(c
        .gateway_release_asset(("o", "r"), mounted.0, mounted.1, "v1", "a.zip")
        .is_err());
    // The asset belongs to a release of another repository than the one asked for.
    assert!(c
        .gateway_release_asset(("x", "y"), HTML, ASSET, "v1", "SHA256SUMS")
        .is_err());
}

#[test]
fn a_release_page_of_another_tag_is_refused() {
    assert!(fjord("https://platform.example")
        .gateway_release_asset(("o", "r"), HTML, ASSET, "v2", "SHA256SUMS")
        .is_err());
}

#[test]
fn a_pat_client_fetches_the_url_directly() {
    let base = Url::parse("https://forge.example/api/v1/").unwrap();
    let pat = Client::for_base_url(base, "pat".into()).unwrap();
    assert!(pat
        .gateway_release_asset(("o", "r"), HTML, ASSET, "v1", "SHA256SUMS")
        .unwrap()
        .is_none());
}

#[test]
fn urls_that_are_not_this_releases_download_route_are_refused() {
    let c = fjord("https://platform.example");
    for (asset, tag, name) in [
        (
            "https://cdn.example/o/r/releases/download/v1/SHA256SUMS",
            "v1",
            "SHA256SUMS",
        ),
        (
            "http://forge.example/o/r/releases/download/v1/SHA256SUMS",
            "v1",
            "SHA256SUMS",
        ),
        (
            "https://forge.example/o/r/releases/download/v2/SHA256SUMS",
            "v1",
            "SHA256SUMS",
        ),
        (
            "https://forge.example/o/r/releases/download/v1/OTHER",
            "v1",
            "SHA256SUMS",
        ),
        (
            "https://forge.example/o/r/raw/branch/main/SHA256SUMS",
            "v1",
            "SHA256SUMS",
        ),
        (
            "https://forge.example/o/r/releases/download/v1/SHA256SUMS?x=1",
            "v1",
            "SHA256SUMS",
        ),
        (
            "https://forge.example/o/r/releases/download/v1/SHA256SUMS#f",
            "v1",
            "SHA256SUMS",
        ),
        (
            "https://forge.example/o/r/releases/download/v1/%2e%2e",
            "v1",
            "..",
        ),
        (
            "https://forge.example/../r/releases/download/v1/a",
            "v1",
            "a",
        ),
    ] {
        assert!(
            c.gateway_release_asset(("o", "r"), HTML, asset, tag, name)
                .is_err(),
            "{asset}"
        );
    }
}

#[tokio::test]
async fn the_asset_is_fetched_from_the_gateway_with_the_bearer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/api/v1/forge-gateway/inst_x/o/r/releases/download/v1/SHA256SUMS",
        ))
        .and(header("authorization", "Bearer jwt"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"abc".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let c = fjord(&server.uri());
    let url = c
        .gateway_release_asset(("o", "r"), HTML, ASSET, "v1", "SHA256SUMS")
        .unwrap()
        .unwrap();
    let res = c.get_release_asset(&url).await.unwrap();
    assert_eq!(res.bytes().await.unwrap().as_ref(), b"abc");
}

#[tokio::test]
async fn a_redirect_to_storage_carries_no_credential() {
    let storage = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/blob"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&storage)
        .await;
    let gateway = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{}/blob", storage.uri())),
        )
        .mount(&gateway)
        .await;
    let c = fjord(&gateway.uri());
    let url = c
        .gateway_release_asset(("o", "r"), HTML, ASSET, "v1", "SHA256SUMS")
        .unwrap()
        .unwrap();
    assert!(c
        .get_release_asset(&url)
        .await
        .unwrap()
        .status()
        .is_success());
    let seen = storage.received_requests().await.unwrap();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].headers.get("authorization").is_none());
}
