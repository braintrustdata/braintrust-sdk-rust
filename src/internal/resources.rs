//! Resource ports adapt SDK policy to the shared one-attempt API contracts.
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
use crate::dataset::{DatasetFetcher, DatasetRegistrar, DatasetSummarizer, DatasetWriter};
use crate::error::{BraintrustError, Result};
use crate::experiments::api::{
    BaseExperimentFetcher, ExperimentComparisonFetcher, ExperimentRegistrar,
};
use crate::experiments::BaseExperimentInfo;
use crate::BraintrustClient;

#[async_trait]
impl ExperimentRegistrar for BraintrustClient {
    async fn register_experiment(
        &self,
        token: &str,
        request: ExperimentRegisterRequest,
    ) -> Result<ExperimentRegisterResponse> {
        self.resource_api(token, None)?
            .experiments()
            .register(request)
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
impl BaseExperimentFetcher for BraintrustClient {
    async fn fetch_base_experiment(
        &self,
        token: &str,
        experiment_id: &str,
    ) -> Result<Option<BaseExperimentInfo>> {
        let response = match self
            .resource_api(token, None)?
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
}

#[async_trait]
impl ExperimentComparisonFetcher for BraintrustClient {
    async fn fetch_experiment_comparison(
        &self,
        token: &str,
        experiment_id: &str,
        base_experiment_id: Option<&str>,
    ) -> Result<ExperimentComparisonResponse> {
        self.resource_api(token, None)?
            .experiments()
            .compare(experiment_id, base_experiment_id)
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
impl DatasetRegistrar for BraintrustClient {
    async fn register_dataset(
        &self,
        token: &str,
        request: DatasetRegisterRequest,
    ) -> Result<DatasetRegisterResponse> {
        self.resource_api(token, Some(&request.org_name))?
            .datasets()
            .register(request)
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
impl DatasetFetcher for BraintrustClient {
    async fn fetch_dataset_records(
        &self,
        token: &str,
        org_name: &str,
        query: BTQLQuery,
    ) -> Result<BTQLResponse> {
        self.resource_api(token, Some(org_name))?
            .btql()
            .query_dataset_records(query)
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
impl DatasetSummarizer for BraintrustClient {
    async fn fetch_dataset_summary(
        &self,
        token: &str,
        org_name: &str,
        dataset_id: &str,
    ) -> Result<DatasetSummaryResponse> {
        self.resource_api(token, Some(org_name))?
            .datasets()
            .summarize(dataset_id)
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
impl DatasetWriter for BraintrustClient {
    async fn write_dataset_record(
        &self,
        token: &str,
        org_name: &str,
        row: DatasetWrite,
    ) -> Result<()> {
        self.resource_api(token, Some(org_name))?
            .datasets()
            .write(&[row])
            .await
            .map_err(Into::into)
    }
}
