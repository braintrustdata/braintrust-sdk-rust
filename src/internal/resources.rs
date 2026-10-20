//! Resource ports adapt SDK policy to the shared one-attempt API contracts.
use crate::internal::execution::ExecutionBinding;
use crate::internal::session::CredentialScope;
use async_trait::async_trait;

use crate::api::{
    btql::{BTQLQuery, BTQLResponse},
    datasets::{
        DatasetRegisterRequest, DatasetRegisterResponse, DatasetSummaryResponse, DatasetWrite,
    },
    experiments::{
        BaseExperimentRequest, ExperimentComparisonResponse, ExperimentRegisterRequest,
        ExperimentRegisterResponse,
    },
};
use crate::error::{BraintrustError, Result};
use crate::experiments::BaseExperimentInfo;
use crate::internal::ports::{DatasetApi, ExperimentApi};

/// Shared HTTP pool and execution lease without client/session/delivery state.
#[derive(Clone)]
pub(crate) struct ResourceTransport {
    http: reqwest::Client,
    execution: ExecutionBinding,
}
impl ResourceTransport {
    pub fn new(http: reqwest::Client, execution: ExecutionBinding) -> Self {
        Self { http, execution }
    }
    fn resource_api(&self, scope: &CredentialScope) -> Result<crate::api::Client> {
        scope.api(&self.http)
    }
}

#[async_trait]
impl ExperimentApi for ResourceTransport {
    async fn register_experiment(
        &self,
        scope: &CredentialScope,
        request: ExperimentRegisterRequest,
    ) -> Result<ExperimentRegisterResponse> {
        // Registration is part of admitted event preparation or an explicitly
        // bound metadata operation; it must remain valid during queue drain.
        self.resource_api(scope)?
            .experiments()
            .register(request)
            .await
            .map_err(BraintrustError::from)
    }

    async fn fetch_base_experiment(
        &self,
        scope: &CredentialScope,
        experiment_id: &str,
    ) -> Result<Option<BaseExperimentInfo>> {
        let api = self.resource_api(scope)?;
        let request = BaseExperimentRequest {
            id: experiment_id.to_owned(),
        };
        self.execution
            .response(async move {
                let response = match api.experiments().base(request).await {
                    // The SDK interprets 400 as no base; the API layer preserves the status.
                    Err(crate::api::Error::Http { status: 400, .. }) => return Ok(None),
                    result => result.map_err(BraintrustError::from)?,
                };
                Ok(match (response.base_exp_id, response.base_exp_name) {
                    (Some(id), Some(name)) => Some(BaseExperimentInfo { id, name }),
                    _ => None,
                })
            })
            .await
    }

    async fn fetch_experiment_comparison(
        &self,
        scope: &CredentialScope,
        experiment_id: &str,
        base_experiment_id: Option<&str>,
    ) -> Result<ExperimentComparisonResponse> {
        let api = self.resource_api(scope)?;
        let experiment_id = experiment_id.to_owned();
        let base_experiment_id = base_experiment_id.map(str::to_owned);
        self.execution
            .response(async move {
                api.experiments()
                    .compare(&experiment_id, base_experiment_id.as_deref())
                    .await
                    .map_err(BraintrustError::from)
            })
            .await
    }
}

// Dataset operations already own one response-task admission for their entire
// registration/request sequence. Subrequests must not reenter admission: they
// remain valid when shutdown closes the public boundary while draining them.
#[async_trait]
impl DatasetApi for ResourceTransport {
    async fn register_dataset(
        &self,
        scope: &CredentialScope,
        request: DatasetRegisterRequest,
    ) -> Result<DatasetRegisterResponse> {
        self.resource_api(scope)?
            .datasets()
            .register(request)
            .await
            .map_err(BraintrustError::from)
    }

    async fn fetch_dataset_records(
        &self,
        scope: &CredentialScope,
        query: BTQLQuery,
    ) -> Result<BTQLResponse> {
        self.resource_api(scope)?
            .btql()
            .query_dataset_records(query)
            .await
            .map_err(BraintrustError::from)
    }

    async fn fetch_dataset_summary(
        &self,
        scope: &CredentialScope,
        dataset_id: &str,
    ) -> Result<DatasetSummaryResponse> {
        self.resource_api(scope)?
            .datasets()
            .summarize(dataset_id)
            .await
            .map_err(BraintrustError::from)
    }

    async fn write_dataset_record(&self, scope: &CredentialScope, row: DatasetWrite) -> Result<()> {
        self.resource_api(scope)?
            .datasets()
            .write(&[row])
            .await
            .map_err(BraintrustError::from)
    }
}
