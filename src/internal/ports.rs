//! Private dependency-injection boundaries for resource I/O and delivery.
use async_trait::async_trait;

use crate::api::{
    btql::{BTQLQuery, BTQLResponse},
    datasets::{
        DatasetRegisterRequest, DatasetRegisterResponse, DatasetSummaryResponse, DatasetWrite,
    },
    experiments::{
        ExperimentComparisonResponse, ExperimentRegisterRequest, ExperimentRegisterResponse,
    },
};
use crate::error::Result;
use crate::experiments::BaseExperimentInfo;
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

/// Shared delivery lifecycle, independent of resource HTTP operations.
#[async_trait]
pub(crate) trait DeliveryControl: Send + Sync {
    fn flush(&self) -> crate::Completion<crate::FlushReport, crate::FlushError>;
    async fn trigger_flush(&self) -> Result<()>;
}

#[async_trait]
pub(crate) trait SpanSubmitter: DeliveryControl {
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

/// One-attempt dataset I/O, polled inside an already admitted resource operation
/// on its execution binding. Subrequests do not reserve additional task slots.
#[async_trait]
pub(crate) trait DatasetApi: Send + Sync {
    async fn register_dataset(
        &self,
        scope: &CredentialScope,
        request: DatasetRegisterRequest,
    ) -> Result<DatasetRegisterResponse>;
    async fn fetch_dataset_records(
        &self,
        scope: &CredentialScope,
        query: BTQLQuery,
    ) -> Result<BTQLResponse>;
    async fn fetch_dataset_summary(
        &self,
        scope: &CredentialScope,
        dataset_id: &str,
    ) -> Result<DatasetSummaryResponse>;
    async fn write_dataset_record(&self, scope: &CredentialScope, row: DatasetWrite) -> Result<()>;
}

/// One-attempt experiment I/O, polled inside admitted preparation or a bound
/// resource operation. HTTP subrequests do not reserve additional task slots.
#[async_trait]
pub(crate) trait ExperimentApi: Send + Sync {
    async fn register_experiment(
        &self,
        scope: &CredentialScope,
        request: ExperimentRegisterRequest,
    ) -> Result<ExperimentRegisterResponse>;
    async fn fetch_base_experiment(
        &self,
        scope: &CredentialScope,
        experiment_id: &str,
    ) -> Result<Option<BaseExperimentInfo>>;
    async fn fetch_experiment_comparison(
        &self,
        scope: &CredentialScope,
        experiment_id: &str,
        base_experiment_id: Option<&str>,
    ) -> Result<ExperimentComparisonResponse>;
}
