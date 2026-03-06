//! Internal API trait definitions for experiments.

use crate::error::Result;
use crate::experiments::api::comparison::ExperimentComparisonResponse;
use crate::experiments::api::registration::{
    ExperimentRegisterRequest, ExperimentRegisterResponse,
};
use crate::experiments::metadata::BaseExperimentInfo;

/// Trait for registering experiments with the Braintrust API.
#[async_trait::async_trait]
pub(crate) trait ExperimentRegistrar: Send + Sync {
    async fn register_experiment(
        &self,
        token: &str,
        request: ExperimentRegisterRequest,
    ) -> Result<ExperimentRegisterResponse>;
}

/// Trait for fetching base experiment information.
#[async_trait::async_trait]
pub(crate) trait BaseExperimentFetcher: Send + Sync {
    async fn fetch_base_experiment(
        &self,
        token: &str,
        experiment_id: &str,
    ) -> Result<Option<BaseExperimentInfo>>;
}

/// Trait for fetching experiment comparison data.
#[async_trait::async_trait]
pub(crate) trait ExperimentComparisonFetcher: Send + Sync {
    async fn fetch_experiment_comparison(
        &self,
        token: &str,
        experiment_id: &str,
        base_experiment_id: Option<&str>,
    ) -> Result<ExperimentComparisonResponse>;
}
