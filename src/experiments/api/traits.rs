//! Internal API trait definitions for experiments.

use crate::error::Result;
use crate::experiments::api::comparison::ExperimentComparisonResponse;
use crate::experiments::api::registration::{
    ExperimentRegisterRequest, ExperimentRegisterResponse,
};
use crate::experiments::metadata::BaseExperimentInfo;
use crate::internal::session::CredentialScope;

/// Trait for registering experiments with the Braintrust API.
#[async_trait::async_trait]
pub(crate) trait ExperimentRegistrar: Send + Sync {
    async fn register_experiment(
        &self,
        scope: &CredentialScope,
        request: ExperimentRegisterRequest,
    ) -> Result<ExperimentRegisterResponse>;
}

/// Trait for fetching base experiment information.
#[async_trait::async_trait]
pub(crate) trait BaseExperimentFetcher: Send + Sync {
    async fn fetch_base_experiment(
        &self,
        scope: &CredentialScope,
        experiment_id: &str,
    ) -> Result<Option<BaseExperimentInfo>>;
}

/// Trait for fetching experiment comparison data.
#[async_trait::async_trait]
pub(crate) trait ExperimentComparisonFetcher: Send + Sync {
    async fn fetch_experiment_comparison(
        &self,
        scope: &CredentialScope,
        experiment_id: &str,
        base_experiment_id: Option<&str>,
    ) -> Result<ExperimentComparisonResponse>;
}
