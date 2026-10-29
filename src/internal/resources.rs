//! Concrete resource HTTP adapter over the shared one-attempt API contracts.
use crate::internal::session::CredentialScope;

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

/// Shared HTTP pool without retaining client/session/delivery state.
#[derive(Clone)]
pub(crate) struct ResourceTransport {
    http: reqwest::Client,
}
impl ResourceTransport {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }
    fn resource_api(&self, scope: &CredentialScope) -> Result<crate::api::Client> {
        scope.api(&self.http)
    }

    pub async fn register_experiment(
        &self,
        scope: &CredentialScope,
        request: ExperimentRegisterRequest,
    ) -> Result<ExperimentRegisterResponse> {
        self.resource_api(scope)?
            .experiments()
            .register(request)
            .await
            .map_err(Into::into)
    }

    pub async fn fetch_base_experiment(
        &self,
        scope: &CredentialScope,
        experiment_id: &str,
    ) -> Result<Option<BaseExperimentInfo>> {
        let response = match self
            .resource_api(scope)?
            .experiments()
            .base(BaseExperimentRequest {
                id: experiment_id.to_owned(),
            })
            .await
        {
            // The SDK interprets 400 as no base; the API layer preserves the status.
            Err(crate::api::Error::Http { status: 400, .. }) => return Ok(None),
            result => result.map_err(BraintrustError::from)?,
        };
        Ok(match (response.base_exp_id, response.base_exp_name) {
            (Some(id), Some(name)) => Some(BaseExperimentInfo { id, name }),
            _ => None,
        })
    }

    pub async fn fetch_experiment_comparison(
        &self,
        scope: &CredentialScope,
        experiment_id: &str,
        base_experiment_id: Option<&str>,
    ) -> Result<ExperimentComparisonResponse> {
        self.resource_api(scope)?
            .experiments()
            .compare(experiment_id, base_experiment_id)
            .await
            .map_err(Into::into)
    }

    pub async fn register_dataset(
        &self,
        scope: &CredentialScope,
        request: DatasetRegisterRequest,
    ) -> Result<DatasetRegisterResponse> {
        self.resource_api(scope)?
            .datasets()
            .register(request)
            .await
            .map_err(Into::into)
    }

    pub async fn fetch_dataset_records(
        &self,
        scope: &CredentialScope,
        query: BTQLQuery,
    ) -> Result<BTQLResponse> {
        self.resource_api(scope)?
            .btql()
            .query_dataset_records(query)
            .await
            .map_err(Into::into)
    }

    pub async fn fetch_dataset_summary(
        &self,
        scope: &CredentialScope,
        dataset_id: &str,
    ) -> Result<DatasetSummaryResponse> {
        self.resource_api(scope)?
            .datasets()
            .summarize(dataset_id)
            .await
            .map_err(Into::into)
    }

    pub async fn write_dataset_record(
        &self,
        scope: &CredentialScope,
        row: DatasetWrite,
    ) -> Result<()> {
        self.resource_api(scope)?
            .datasets()
            .write(&[row])
            .await
            .map_err(Into::into)
    }
}
