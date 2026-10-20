//! Existing experiment registration, base and comparison endpoint contracts.
use super::{Client, Error, Plane, Transport};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Repository information for experiment tracking.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RepoInfo {
    /// Git commit SHA.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Current branch name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Current tag (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Whether the working directory has uncommitted changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    /// Git author name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    /// Git author email.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_email: Option<String>,
    /// Git commit message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<String>,
    /// Git commit timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_time: Option<String>,
}

/// Request body for experiment registration.
#[derive(Debug, Clone, Serialize)]
pub struct ExperimentRegisterRequest {
    pub project_name: String,
    pub org_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experiment_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_experiment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_info: Option<RepoInfo>,
}

/// Response from experiment registration.
#[derive(Debug, Clone, Deserialize)]
pub struct ExperimentRegisterResponse {
    pub project: ProjectInfo,
    pub experiment: ExperimentInfo,
}

/// Project information from registration response.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
}

/// Experiment information from registration response.
#[derive(Debug, Clone, Deserialize)]
pub struct ExperimentInfo {
    pub id: String,
    pub name: String,
}

/// Request body for fetching base experiment.
#[derive(Debug, Clone, Serialize)]
pub struct BaseExperimentRequest {
    pub id: String,
}

/// Response from base experiment fetch.
#[derive(Debug, Clone, Deserialize)]
pub struct BaseExperimentResponse {
    pub base_exp_id: Option<String>,
    pub base_exp_name: Option<String>,
}
/// Response from experiment comparison API.
#[derive(Debug, Clone, Deserialize)]
pub struct ExperimentComparisonResponse {
    #[serde(default)]
    pub scores: HashMap<String, ComparisonScoreData>,
    #[serde(default)]
    pub metrics: HashMap<String, ComparisonMetricData>,
    pub base_experiment_name: Option<String>,
}

/// Score data from comparison API.
#[derive(Debug, Clone, Deserialize)]
pub struct ComparisonScoreData {
    pub score: f64,
    pub diff: Option<f64>,
    pub improvements: Option<u32>,
    pub regressions: Option<u32>,
}

/// Metric data from comparison API.
#[derive(Debug, Clone, Deserialize)]
pub struct ComparisonMetricData {
    pub metric: f64,
    pub unit: Option<String>,
    pub diff: Option<f64>,
    pub improvements: Option<u32>,
    pub regressions: Option<u32>,
}

#[derive(Clone)]
pub struct Experiments {
    pub(crate) client: Client,
}
impl Experiments {
    /// Register one experiment on the control plane.
    /// Submission is immediate; dropping the token does not cancel accepted work.
    pub fn register(
        &self,
        request: ExperimentRegisterRequest,
    ) -> crate::Completion<ExperimentRegisterResponse, Error> {
        self.client
            .submit(move |transport| async move { transport.register_experiment(request).await })
    }
    /// Fetch the base experiment, preserving all endpoint errors including HTTP 400.
    /// Submission is immediate; dropping the token does not cancel accepted work.
    pub fn base(
        &self,
        request: BaseExperimentRequest,
    ) -> crate::Completion<BaseExperimentResponse, Error> {
        self.client
            .submit(move |transport| async move { transport.base_experiment(request).await })
    }
    /// Fetch one experiment comparison.
    /// Submission is immediate; dropping the token does not cancel accepted work.
    pub fn compare(
        &self,
        experiment_id: &str,
        base_experiment_id: Option<&str>,
    ) -> crate::Completion<ExperimentComparisonResponse, Error> {
        self.client.submit(move |transport| {
            let experiment_id = experiment_id.to_owned();
            let base_experiment_id = base_experiment_id.map(str::to_owned);
            async move {
                transport
                    .compare_experiments(&experiment_id, base_experiment_id.as_deref())
                    .await
            }
        })
    }
}

impl Transport {
    pub(crate) async fn register_experiment(
        &self,
        request: ExperimentRegisterRequest,
    ) -> Result<ExperimentRegisterResponse, Error> {
        self.post_json(Plane::Control, "api/experiment/register", &request)
            .await
    }
    /// Return the actual endpoint result, including a 400 response; SDK interpretation belongs above this layer.
    pub(crate) async fn base_experiment(
        &self,
        request: BaseExperimentRequest,
    ) -> Result<BaseExperimentResponse, Error> {
        self.post_json(Plane::Control, "api/base_experiment/get_id", &request)
            .await
    }
    pub(crate) async fn compare_experiments(
        &self,
        experiment_id: &str,
        base_experiment_id: Option<&str>,
    ) -> Result<ExperimentComparisonResponse, Error> {
        let mut query = vec![("experiment_id", experiment_id)];
        if let Some(base) = base_experiment_id {
            query.push(("base_experiment_id", base));
        }
        self.get_json(Plane::Control, "experiment-comparison2", &query)
            .await
    }
}
