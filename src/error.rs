use thiserror::Error;

#[derive(Debug, Error)]
pub enum BraintrustError {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("network error: {0}")]
    Network(String),
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("internal channel closed")]
    ChannelClosed,
    #[error("background task failed: {0}")]
    Background(String),
    #[error("stream aggregation error: {0}")]
    StreamAggregation(String),
}

pub type Result<T> = std::result::Result<T, BraintrustError>;
