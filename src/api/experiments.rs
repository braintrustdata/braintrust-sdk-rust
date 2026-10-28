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
    pub(crate) commit: Option<String>,
    /// Current branch name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) branch: Option<String>,
    /// Current tag (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tag: Option<String>,
    /// Whether the working directory has uncommitted changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dirty: Option<bool>,
    /// Git author name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) author_name: Option<String>,
    /// Git author email.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) author_email: Option<String>,
    /// Git commit message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) commit_message: Option<String>,
    /// Git commit timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) commit_time: Option<String>,
}

/// Request body for experiment registration.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ExperimentRegisterRequest {
    pub(crate) project_name: String,
    pub(crate) org_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) experiment_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
    /// Set the commit.
    pub fn set_commit(&mut self, value: Option<String>) -> &mut Self {
        self.commit = value;
        self
    }
    /// Return the branch.
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }
    /// Set the branch.
    pub fn set_branch(&mut self, value: Option<String>) -> &mut Self {
        self.branch = value;
        self
    }
    /// Return the tag.
    pub fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }
    /// Set the tag.
    pub fn set_tag(&mut self, value: Option<String>) -> &mut Self {
        self.tag = value;
        self
    }
    /// Return the dirty.
    pub fn dirty(&self) -> Option<&bool> {
        self.dirty.as_ref()
    }
    /// Set the dirty.
    pub fn set_dirty(&mut self, value: Option<bool>) -> &mut Self {
        self.dirty = value;
        self
    }
    /// Return the author name.
    pub fn author_name(&self) -> Option<&str> {
        self.author_name.as_deref()
    }
    /// Set the author name.
    pub fn set_author_name(&mut self, value: Option<String>) -> &mut Self {
        self.author_name = value;
        self
    }
    /// Return the author email.
    pub fn author_email(&self) -> Option<&str> {
        self.author_email.as_deref()
    }
    /// Set the author email.
    pub fn set_author_email(&mut self, value: Option<String>) -> &mut Self {
        self.author_email = value;
        self
    }
    /// Return the commit message.
    pub fn commit_message(&self) -> Option<&str> {
        self.commit_message.as_deref()
    }
    /// Set the commit message.
    pub fn set_commit_message(&mut self, value: Option<String>) -> &mut Self {
        self.commit_message = value;
        self
    }
    /// Return the commit time.
    pub fn commit_time(&self) -> Option<&str> {
        self.commit_time.as_deref()
    }
    /// Set the commit time.
    pub fn set_commit_time(&mut self, value: Option<String>) -> &mut Self {
        self.commit_time = value;
        self
    }
}

impl ExperimentRegisterRequest {
    /// Return the project name.
    pub fn project_name(&self) -> &str {
        &self.project_name
    }
    /// Set the project name.
    pub fn set_project_name(&mut self, value: impl Into<String>) -> &mut Self {
        self.project_name = value.into();
        self
    }
    /// Return the org id.
    pub fn org_id(&self) -> &str {
        &self.org_id
    }
    /// Set the org id.
    pub fn set_org_id(&mut self, value: impl Into<String>) -> &mut Self {
        self.org_id = value.into();
        self
    }
    /// Return the experiment name.
    pub fn experiment_name(&self) -> Option<&str> {
        self.experiment_name.as_deref()
    }
    /// Set the experiment name.
    pub fn set_experiment_name(&mut self, value: Option<String>) -> &mut Self {
        self.experiment_name = value;
        self
    }
    /// Return the description.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
    /// Set the description.
    pub fn set_description(&mut self, value: Option<String>) -> &mut Self {
        self.description = value;
        self
    }
    /// Return the base experiment.
    pub fn base_experiment(&self) -> Option<&str> {
        self.base_experiment.as_deref()
    }
    /// Set the base experiment.
    pub fn set_base_experiment(&mut self, value: Option<String>) -> &mut Self {
        self.base_experiment = value;
        self
    }
    /// Return the metadata.
    pub fn metadata(&self) -> Option<&Map<String, Value>> {
        self.metadata.as_ref()
    }
    /// Set the metadata.
    pub fn set_metadata(&mut self, value: Option<Map<String, Value>>) -> &mut Self {
        self.metadata = value;
        self
    }
    /// Return the public.
    pub fn public(&self) -> Option<&bool> {
        self.public.as_ref()
    }
    /// Set the public.
    pub fn set_public(&mut self, value: Option<bool>) -> &mut Self {
        self.public = value;
        self
    }
    /// Return the update.
    pub fn update(&self) -> Option<&bool> {
        self.update.as_ref()
    }
    /// Set the update.
    pub fn set_update(&mut self, value: Option<bool>) -> &mut Self {
        self.update = value;
        self
    }
    /// Return the repo info.
    pub fn repo_info(&self) -> Option<&RepoInfo> {
        self.repo_info.as_ref()
    }
    /// Set the repo info.
    pub fn set_repo_info(&mut self, value: Option<RepoInfo>) -> &mut Self {
        self.repo_info = value;
        self
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
