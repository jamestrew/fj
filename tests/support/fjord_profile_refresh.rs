#[path = "fjord_profiles.rs"]
mod profiles;
pub use profiles::*;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

use std::path::Path;

pub fn config(dir: &Path) -> toml::Value {
    std::fs::read_to_string(dir.join("hosts.toml"))
        .unwrap()
        .parse()
        .unwrap()
}

pub async fn expire(server: &MockServer, bearer: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v1/me/forge-instances"))
        .and(header("authorization", format!("Bearer {bearer}")))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(1)
        .mount(server)
        .await;
}
