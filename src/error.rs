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
    #[error(transparent)]
    Execution(#[from] crate::ExecutionError),
    #[error(transparent)]
    Admission(#[from] crate::EnqueueError),
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
        match &error {
            crate::FlushError::Admission(error) => return Self::Admission(*error),
            crate::FlushError::Execution(error) => return Self::Execution(*error),
            crate::FlushError::Delivery(_) => {}
        }
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
        match &error {
            crate::WriteError::Admission(error) => return Self::Admission(*error),
            crate::WriteError::Execution(error) => return Self::Execution(*error),
            crate::WriteError::Delivery(_) => {}
        }
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
