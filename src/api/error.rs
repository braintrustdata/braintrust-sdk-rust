use thiserror::Error;

/// Direct request failures retain their operation, status and original source.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    #[error("invalid API configuration: {0}")]
    Config(String),
    #[error("{operation}: transport failed")]
    #[non_exhaustive]
    Transport {
        operation: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("{operation}: HTTP {status}")]
    #[non_exhaustive]
    Http {
        operation: &'static str,
        status: u16,
        /// Server response retained for programmatic inspection, not Display.
        body: String,
    },
    #[error("{operation}: invalid JSON response (HTTP {status})")]
    #[non_exhaustive]
    Decode {
        operation: &'static str,
        status: u16,
        #[source]
        source: serde_json::Error,
    },
}
