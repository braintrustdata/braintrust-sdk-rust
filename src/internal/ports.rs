//! Private dependency-injection boundaries for span delivery.
use async_trait::async_trait;

use crate::error::Result;
use crate::internal::session::CredentialScope;
use crate::types::{ParentSpanInfo, SpanPayload};

/// Shared delivery lifecycle, independent of resource HTTP operations.
#[async_trait]
pub(crate) trait DeliveryControl: Send + Sync {
    fn flush(&self) -> crate::Completion<crate::FlushReport, crate::FlushError>;
    async fn trigger_flush(&self) -> Result<()>;
}

#[async_trait]
pub(crate) trait SpanSubmitter: DeliveryControl {
    fn submit(
        &self,
        scope: CredentialScope,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> crate::Completion<crate::DeliveryReceipt, crate::WriteError>;
}
