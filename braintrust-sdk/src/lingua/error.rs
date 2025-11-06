//! Error types for the Lingua LLM client

use thiserror::Error;

/// Errors that can occur when using the Lingua LLM client
#[derive(Error, Debug)]
pub enum LinguaError {
    /// Failed to convert messages between formats
    #[error("Failed to convert messages: {0}")]
    MessageConversion(String),

    /// HTTP request to LLM failed
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    /// Failed to parse LLM response
    #[error("Failed to parse response: {0}")]
    ResponseParsing(String),

    /// LLM API returned an error
    #[error("LLM request failed ({status_code}): {body}")]
    LlmRequestError {
        status_code: reqwest::StatusCode,
        body: String,
    },

    /// Missing required environment variable
    #[error("Missing environment variable: {0}")]
    MissingEnvVar(String),

    /// JSON serialization/deserialization error
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    /// No output text found in response
    #[error("No output text found in LLM response")]
    NoOutputText,

    /// Braintrust SDK error
    #[error("Braintrust SDK error: {0}")]
    BraintrustError(#[from] crate::error::BraintrustError),
}

/// Result type alias for Lingua operations
pub type Result<T> = std::result::Result<T, LinguaError>;
