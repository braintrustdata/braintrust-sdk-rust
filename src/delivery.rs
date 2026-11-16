//! Outcomes of queued ingestion and its lifecycle operations.
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum EnqueueError {
    #[error("delivery admission capacity is full")]
    Full,
    #[error("delivery queue is closed")]
    Closed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ExecutionError {
    #[error("delivery worker stopped before completing accepted work")]
    WorkerLost,
    #[error("delivery shutdown timed out")]
    TimedOut,
}
/// A terminal preparation, encoding or upload failure shared by a logical batch.
#[derive(Debug, Error)]
#[error("log delivery failed: {source}")]
pub struct DeliveryError {
    #[source]
    pub(crate) source: anyhow::Error,
}
impl DeliveryError {
    /// Inspect the original preparation, encoding or transport error.
    pub fn cause(&self) -> &anyhow::Error {
        &self.source
    }
}
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum WriteError {
    #[error(transparent)]
    Admission(#[from] EnqueueError),
    #[error(transparent)]
    Execution(#[from] ExecutionError),
    #[error(transparent)]
    Delivery(Arc<DeliveryError>),
}
/// Acknowledgment that the logical batch containing this write succeeded.
/// Backend query visibility is separate from delivery acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryReceipt {
    pub(crate) _private: (),
}
impl DeliveryReceipt {
    pub(crate) fn new() -> Self {
        Self { _private: () }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FlushReport {
    /// Accepted operations delivered through this flush's cutoff, before merging.
    pub(crate) delivered: u64,
}
impl FlushReport {
    /// Number of accepted operations delivered through the flush cutoff.
    pub fn delivered(&self) -> u64 {
        self.delivered
    }
}
#[derive(Debug, Clone, Error)]
#[error("{failed} accepted writes failed: {cause}")]
pub struct FlushFailure {
    pub(crate) failed: u64,
    pub(crate) cause: Arc<DeliveryError>,
}
impl FlushFailure {
    /// Number of accepted writes that failed.
    pub fn failed(&self) -> u64 {
        self.failed
    }
    /// Shared cause of the failed writes.
    pub fn cause(&self) -> &Arc<DeliveryError> {
        &self.cause
    }
}
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum FlushError {
    #[error(transparent)]
    Admission(#[from] EnqueueError),
    #[error(transparent)]
    Execution(#[from] ExecutionError),
    #[error(transparent)]
    Delivery(FlushFailure),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShutdownReport {
    pub(crate) delivered: u64,
}
impl ShutdownReport {
    /// Number of accepted operations delivered through the shutdown cutoff.
    pub fn delivered(&self) -> u64 {
        self.delivered
    }
}
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum ShutdownError {
    #[error(transparent)]
    Admission(#[from] EnqueueError),
    #[error(transparent)]
    Execution(#[from] ExecutionError),
    #[error(transparent)]
    Delivery(FlushFailure),
}
impl From<FlushError> for ShutdownError {
    fn from(error: FlushError) -> Self {
        match error {
            FlushError::Admission(error) => Self::Admission(error),
            FlushError::Execution(error) => Self::Execution(error),
            FlushError::Delivery(error) => Self::Delivery(error),
        }
    }
}
