//! Existing experiment registration, base and comparison endpoint contracts.
use super::{Client, Error, Plane};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Repository information for experiment tracking.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
pub struct ExperimentRegisterResponse {
    pub project: ProjectInfo,
    pub experiment: ExperimentInfo,
}

/// Project information from registration response.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
}

/// Experiment information from registration response.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ExperimentInfo {
    pub id: String,
    pub name: String,
}

/// Request body for fetching base experiment.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct BaseExperimentRequest {
    pub id: String,
}

/// Response from base experiment fetch.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct BaseExperimentResponse {
    pub base_exp_id: Option<String>,
    pub base_exp_name: Option<String>,
}
/// Response from experiment comparison API.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ExperimentComparisonResponse {
    #[serde(default)]
    pub scores: HashMap<String, ComparisonScoreData>,
    #[serde(default)]
    pub metrics: HashMap<String, ComparisonMetricData>,
    pub base_experiment_name: Option<String>,
}

/// Score data from comparison API.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ComparisonScoreData {
    pub score: f64,
    pub diff: Option<f64>,
    pub improvements: Option<u32>,
    pub regressions: Option<u32>,
}

/// Metric data from comparison API.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
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
    pub async fn register(
        &self,
        request: ExperimentRegisterRequest,
    ) -> Result<ExperimentRegisterResponse, Error> {
        self.client
            .post_json(Plane::Control, "api/experiment/register", &request)
            .await
    }
    /// Return the actual endpoint result, including a 400 response; SDK interpretation belongs above this layer.
    pub async fn base(
        &self,
        request: BaseExperimentRequest,
    ) -> Result<BaseExperimentResponse, Error> {
        self.client
            .post_json(Plane::Control, "api/base_experiment/get_id", &request)
            .await
    }
    pub async fn compare(
        &self,
        experiment_id: &str,
        base_experiment_id: Option<&str>,
    ) -> Result<ExperimentComparisonResponse, Error> {
        let mut query = vec![("experiment_id", experiment_id)];
        if let Some(base) = base_experiment_id {
            query.push(("base_experiment_id", base));
        }
        self.client
            .get_json(Plane::Control, "experiment-comparison2", &query)
            .await
    }
}

impl ExperimentRegisterRequest {
    /// Construct a registration request with required project name and organization ID.
    pub fn new(project_name: impl Into<String>, org_id: impl Into<String>) -> Self {
        Self {
            project_name: project_name.into(),
            org_id: org_id.into(),
            experiment_name: None,
            description: None,
            base_experiment: None,
            metadata: None,
            public: None,
            update: None,
            repo_info: None,
        }
    }
}
impl BaseExperimentRequest {
    /// Construct a base experiment lookup for an experiment ID.
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}
