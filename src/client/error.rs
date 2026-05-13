use reqwest::StatusCode;
use thiserror::Error;

/// A non-2xx response from the Forgejo API. We surface the parsed `message`
/// when one is available and keep the raw body for `--debug` style output.
#[derive(Debug, Error)]
#[error("HTTP {status} from {url}: {message}")]
pub struct ApiError {
    pub status: StatusCode,
    pub url: String,
    pub message: String,
    pub body: String,
}
