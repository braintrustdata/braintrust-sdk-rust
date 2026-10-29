use thiserror::Error;

/// SDK failures. Match known cases with a fallback for future variants.
///
/// ```compile_fail
/// use braintrust_sdk_rust::BraintrustError;
/// fn consume(error: BraintrustError) {
///     match error {
///         BraintrustError::InvalidConfig(_) | BraintrustError::Network(_)
///         | BraintrustError::Background(_) | BraintrustError::StreamAggregation(_)
///         | BraintrustError::Http(_) | BraintrustError::Api { .. }
///         | BraintrustError::ChannelClosed => {}
///     }
/// }
/// ```
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BraintrustError {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("network error: {0}")]
    Network(String),
    /// API failures may gain additional diagnostic fields.
    ///
    /// ```compile_fail
    /// use braintrust_sdk_rust::BraintrustError;
    /// fn consume(error: BraintrustError) {
    ///     if let BraintrustError::Api { status, message } = error {
    ///         let _ = (status, message);
    ///     }
    /// }
    /// ```
    #[error("API error ({status}): {message}")]
    #[non_exhaustive]
    Api { status: u16, message: String },
    #[error("internal channel closed")]
    ChannelClosed,
    #[error("background task failed: {0}")]
    Background(String),
    #[error("stream aggregation error: {0}")]
    StreamAggregation(String),
}

pub type Result<T> = std::result::Result<T, BraintrustError>;

// Compatibility for resource APIs that still return BraintrustError while the
// public operation surfaces migrate to typed completion errors.
impl From<crate::FlushError> for BraintrustError {
    fn from(error: crate::FlushError) -> Self {
        if let crate::FlushError::Delivery(failure) = &error {
            if let Some(Self::Api { status, message }) =
                failure.cause.cause().downcast_ref::<Self>()
            {
                return Self::Api {
                    status: *status,
                    message: message.clone(),
                };
            }
        }
        Self::Background(error.to_string())
    }
}

impl From<crate::WriteError> for BraintrustError {
    fn from(error: crate::WriteError) -> Self {
        if let crate::WriteError::Delivery(cause) = &error {
            if let Some(Self::Api { status, message }) = cause.cause().downcast_ref::<Self>() {
                return Self::Api {
                    status: *status,
                    message: message.clone(),
                };
            }
        }
        Self::Background(error.to_string())
    }
}

impl From<crate::api::Error> for BraintrustError {
    fn from(error: crate::api::Error) -> Self {
        match error {
            crate::api::Error::Config(message) => BraintrustError::InvalidConfig(message),
            crate::api::Error::Transport { source, .. } => BraintrustError::Http(source),
            crate::api::Error::Http { status, body, .. } => BraintrustError::Api {
                status,
                message: body,
            },
            crate::api::Error::Decode {
                operation,
                status,
                source,
            } => BraintrustError::Api {
                status,
                message: format!("{operation}: invalid response: {source}"),
            },
        }
    }
}
