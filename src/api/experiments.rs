//! Existing experiment registration, base and comparison endpoint contracts.
use super::{Client, Error, Plane};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Repository information for experiment tracking.
#[derive(bon::Builder, Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct RepoInfo {
    /// Git commit SHA.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) commit: Option<String>,
    /// Current branch name.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) branch: Option<String>,
    /// Current tag (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) tag: Option<String>,
    /// Whether the working directory has uncommitted changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dirty: Option<bool>,
    /// Git author name.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) author_name: Option<String>,
    /// Git author email.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) author_email: Option<String>,
    /// Git commit message.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) commit_message: Option<String>,
    /// Git commit timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) commit_time: Option<String>,
}

/// Request body for experiment registration.
#[derive(bon::Builder, Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ExperimentRegisterRequest {
    #[builder(into)]
    pub(crate) project_name: String,
    #[builder(into)]
    pub(crate) org_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) experiment_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) base_experiment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) metadata: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) public: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) update: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) repo_info: Option<RepoInfo>,
}

/// Response from experiment registration.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ExperimentRegisterResponse {
    pub(crate) project: ProjectInfo,
    pub(crate) experiment: ExperimentInfo,
}

/// Project information from registration response.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ProjectInfo {
    pub(crate) id: String,
    pub(crate) name: String,
}

/// Experiment information from registration response.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ExperimentInfo {
    pub(crate) id: String,
    pub(crate) name: String,
}

/// Request body for fetching base experiment.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct BaseExperimentRequest {
    pub(crate) id: String,
}

/// Response from base experiment fetch.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct BaseExperimentResponse {
    pub(crate) base_exp_id: Option<String>,
    pub(crate) base_exp_name: Option<String>,
}
/// Response from experiment comparison API.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ExperimentComparisonResponse {
    #[serde(default)]
    pub(crate) scores: HashMap<String, ComparisonScoreData>,
    #[serde(default)]
    pub(crate) metrics: HashMap<String, ComparisonMetricData>,
    pub(crate) base_experiment_name: Option<String>,
}

/// Score data from comparison API.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ComparisonScoreData {
    pub(crate) score: f64,
    pub(crate) diff: Option<f64>,
    pub(crate) improvements: Option<u32>,
    pub(crate) regressions: Option<u32>,
}

/// Metric data from comparison API.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ComparisonMetricData {
    pub(crate) metric: f64,
    pub(crate) unit: Option<String>,
    pub(crate) diff: Option<f64>,
    pub(crate) improvements: Option<u32>,
    pub(crate) regressions: Option<u32>,
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

impl RepoInfo {
    /// Return the commit.
    pub fn commit(&self) -> Option<&str> {
        self.commit.as_deref()
    }
    /// Return the branch.
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }
    /// Return the tag.
    pub fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }
    /// Return the dirty.
    pub fn dirty(&self) -> Option<&bool> {
        self.dirty.as_ref()
    }
    /// Return the author name.
    pub fn author_name(&self) -> Option<&str> {
        self.author_name.as_deref()
    }
    /// Return the author email.
    pub fn author_email(&self) -> Option<&str> {
        self.author_email.as_deref()
    }
    /// Return the commit message.
    pub fn commit_message(&self) -> Option<&str> {
        self.commit_message.as_deref()
    }
    /// Return the commit time.
    pub fn commit_time(&self) -> Option<&str> {
        self.commit_time.as_deref()
    }
}

impl ExperimentRegisterRequest {
    /// Return the project name.
    pub fn project_name(&self) -> &str {
        &self.project_name
    }
    /// Return the org id.
    pub fn org_id(&self) -> &str {
        &self.org_id
    }
    /// Return the experiment name.
    pub fn experiment_name(&self) -> Option<&str> {
        self.experiment_name.as_deref()
    }
    /// Return the description.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
    /// Return the base experiment.
    pub fn base_experiment(&self) -> Option<&str> {
        self.base_experiment.as_deref()
    }
    /// Return the metadata.
    pub fn metadata(&self) -> Option<&Map<String, Value>> {
        self.metadata.as_ref()
    }
    /// Return the public.
    pub fn public(&self) -> Option<&bool> {
        self.public.as_ref()
    }
    /// Return the update.
    pub fn update(&self) -> Option<&bool> {
        self.update.as_ref()
    }
    /// Return the repo info.
    pub fn repo_info(&self) -> Option<&RepoInfo> {
        self.repo_info.as_ref()
    }
}

impl ExperimentRegisterResponse {
    /// Return the project.
    pub fn project(&self) -> &ProjectInfo {
        &self.project
    }
    /// Return the experiment.
    pub fn experiment(&self) -> &ExperimentInfo {
        &self.experiment
    }
}

impl ProjectInfo {
    /// Return the id.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Return the name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl ExperimentInfo {
    /// Return the id.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Return the name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl BaseExperimentRequest {
    /// Return the id.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Set the id.
    pub fn set_id(&mut self, value: impl Into<String>) -> &mut Self {
        self.id = value.into();
        self
    }
}

impl BaseExperimentResponse {
    /// Return the base exp id.
    pub fn base_exp_id(&self) -> Option<&str> {
        self.base_exp_id.as_deref()
    }
    /// Return the base exp name.
    pub fn base_exp_name(&self) -> Option<&str> {
        self.base_exp_name.as_deref()
    }
}

impl ExperimentComparisonResponse {
    /// Return the scores.
    pub fn scores(&self) -> &HashMap<String, ComparisonScoreData> {
        &self.scores
    }
    /// Return the metrics.
    pub fn metrics(&self) -> &HashMap<String, ComparisonMetricData> {
        &self.metrics
    }
    /// Return the base experiment name.
    pub fn base_experiment_name(&self) -> Option<&str> {
        self.base_experiment_name.as_deref()
    }
}

impl ComparisonScoreData {
    /// Return the score.
    pub fn score(&self) -> f64 {
        self.score
    }
    /// Return the diff.
    pub fn diff(&self) -> Option<&f64> {
        self.diff.as_ref()
    }
    /// Return the improvements.
    pub fn improvements(&self) -> Option<&u32> {
        self.improvements.as_ref()
    }
    /// Return the regressions.
    pub fn regressions(&self) -> Option<&u32> {
        self.regressions.as_ref()
    }
}

impl ComparisonMetricData {
    /// Return the metric.
    pub fn metric(&self) -> f64 {
        self.metric
    }
    /// Return the unit.
    pub fn unit(&self) -> Option<&str> {
        self.unit.as_deref()
    }
    /// Return the diff.
    pub fn diff(&self) -> Option<&f64> {
        self.diff.as_ref()
    }
    /// Return the improvements.
    pub fn improvements(&self) -> Option<&u32> {
        self.improvements.as_ref()
    }
    /// Return the regressions.
    pub fn regressions(&self) -> Option<&u32> {
        self.regressions.as_ref()
    }
}
