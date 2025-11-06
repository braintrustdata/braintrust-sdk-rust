//! Error types for the Braintrust SDK

use thiserror::Error;

/// The main error type for Braintrust SDK operations
#[derive(Error, Debug)]
pub enum BraintrustError {
    /// HTTP request failed
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    /// Authentication failed
    #[error("Authentication failed: {0}")]
    AuthError(String),

    /// Serialization/deserialization error
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    ConfigError(String),

    /// API returned an error
    #[error("API error: {status} - {message}")]
    ApiError {
        status: u16,
        message: String,
    },

    /// Span operation failed
    #[error("Span error: {0}")]
    SpanError(String),

    /// Queue operation failed
    #[error("Queue error: {0}")]
    QueueError(String),

    /// Invalid UUID
    #[error("Invalid UUID: {0}")]
    InvalidUuid(#[from] uuid::Error),

    /// IO error
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// Generic error
    #[error("{0}")]
    Other(String),
}

/// Result type alias for Braintrust operations
pub type Result<T> = std::result::Result<T, BraintrustError>;
