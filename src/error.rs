use thiserror::Error;

#[derive(Debug, Error)]
pub enum BraintrustError {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("internal channel closed")]
    ChannelClosed,
    #[error("background task failed: {0}")]
    Background(String),
}

pub type Result<T> = std::result::Result<T, BraintrustError>;

