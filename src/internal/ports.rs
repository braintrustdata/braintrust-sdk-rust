//! Private dependency-injection boundaries for span delivery.

use crate::error::Result;
use crate::internal::session::CredentialScope;
use crate::internal::shared_outcome::WriteOutcome;
use crate::types::{ParentSpanInfo, SpanPayload};

/// Fully scoped input to row preparation. Unresolved identity stays in the
/// preparation future rather than being represented by placeholder credentials.
pub(crate) struct PreparedSpan {
    pub scope: CredentialScope,
    pub payload: SpanPayload,
    pub parent_info: Option<ParentSpanInfo>,
}

pub(crate) struct SpanOperation {
    pub submitter: std::sync::Arc<dyn SpanSubmitter>,
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}
impl SpanOperation {
    pub fn new(
        submitter: std::sync::Arc<dyn SpanSubmitter>,
        release: impl FnOnce() + Send + Sync + 'static,
    ) -> Self {
        Self {
            submitter,
            release: Some(Box::new(release)),
        }
    }
}
impl Drop for SpanOperation {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

pub(crate) trait SpanSubmitter: Send + Sync {
    fn flush(&self) -> crate::Completion<crate::FlushReport, crate::FlushError>;
    fn operation(&self) -> std::result::Result<SpanOperation, crate::EnqueueError>;

    /// Queue the write now and resolve its immutable scope/destination during
    /// preparation. Rejected work drops the future without polling it.
    fn submit_deferred(
        &self,
        row_id: String,
        prepare: futures::future::BoxFuture<'static, Result<PreparedSpan>>,
    ) -> crate::Completion<crate::DeliveryReceipt, crate::WriteError>;

    fn submit(
        &self,
        scope: CredentialScope,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
        outcome: Option<WriteOutcome>,
    ) -> crate::Completion<crate::DeliveryReceipt, crate::WriteError>;
}
