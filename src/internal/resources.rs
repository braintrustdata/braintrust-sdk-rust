//! Resource policy over the generated API client. No HTTP request construction.
use crate::api::{
    DatasetRegisterRequest, DatasetRegisterResponse, DatasetSummaryResponse,
    ExperimentComparisonResponse, ExperimentRegisterRequest, ExperimentRegisterResponse,
};
use crate::dataset::{BTQLQuery, BTQLResponse, DatasetWrite};
use crate::error::{BraintrustError, Result};
use crate::experiments::BaseExperimentInfo;
use crate::http::api_result;
use crate::internal::session::CredentialScope;
#[derive(Clone)]
pub(crate) struct ResourceTransport {
    http: reqwest::Client,
}
impl ResourceTransport {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }
    pub async fn register_experiment(
        &self,
        scope: &CredentialScope,
        request: ExperimentRegisterRequest,
    ) -> Result<ExperimentRegisterResponse> {
        Ok(
            api_result(scope.api(&self.http)?.register_experiment(&request).await)
                .await?
                .into_inner(),
        )
    }
    pub async fn fetch_base_experiment(
        &self,
        scope: &CredentialScope,
        id: &str,
    ) -> Result<Option<BaseExperimentInfo>> {
        let result = api_result(scope.api(&self.http)?.base_experiment(id).await).await;
        let response = match result {
            Ok(value) => value.into_inner(),
            Err(BraintrustError::Api { status: 400, .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(match (response.base_exp_id, response.base_exp_name) {
            (Some(id), Some(name)) => Some(BaseExperimentInfo { id, name }),
            _ => None,
        })
    }
    pub async fn fetch_experiment_comparison(
        &self,
        scope: &CredentialScope,
        id: &str,
        base: Option<&str>,
    ) -> Result<ExperimentComparisonResponse> {
        Ok(
            api_result(scope.api(&self.http)?.experiment_comparison(id, base).await)
                .await?
                .into_inner(),
        )
    }
    pub async fn register_dataset(
        &self,
        scope: &CredentialScope,
        request: DatasetRegisterRequest,
    ) -> Result<DatasetRegisterResponse> {
        Ok(
            api_result(scope.api(&self.http)?.register_dataset(&request).await)
                .await?
                .into_inner(),
        )
    }
    pub async fn fetch_dataset_records(
        &self,
        scope: &CredentialScope,
        query: BTQLQuery,
    ) -> Result<BTQLResponse> {
        Ok(api_result(
            scope
                .api(&self.http)?
                .btql_json_as::<_, BTQLResponse>(&query)
                .await,
        )
        .await?
        .into_inner())
    }
    pub async fn fetch_dataset_summary(
        &self,
        scope: &CredentialScope,
        id: &str,
    ) -> Result<DatasetSummaryResponse> {
        Ok(api_result(scope.api(&self.http)?.dataset_summary(id).await)
            .await?
            .into_inner())
    }
    pub async fn write_dataset_record(
        &self,
        scope: &CredentialScope,
        row: DatasetWrite,
    ) -> Result<()> {
        api_result(
            scope
                .api(&self.http)?
                .logs3(&serde_json::json!({"api_version":2,"rows":[row]}))
                .await,
        )
        .await?;
        Ok(())
    }
}
