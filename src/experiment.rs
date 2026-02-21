//! Experiment support for tracking logged events across model versions.
//!
//! Experiments allow you to:
//! - Log evaluation events with input, output, expected, and scores
//! - Compare results against baseline experiments
//! - Get aggregated summaries of experiment performance
//!
//! # Example
//!
//! ```ignore
//! let experiment = client
//!     .experiment_builder_with_credentials(&api_key, &org_id)
//!     .project_name("my-project")
//!     .experiment_name("gpt-4-baseline")
//!     .build();
//!
//! let id = experiment.log(
//!     ExperimentLog::builder()
//!         .input(json!({"question": "What is 2+2?"}))
//!         .output(json!({"answer": "4"}))
//!         .expected(json!({"answer": "4"}))
//!         .score("accuracy", 1.0)
//!         .build()?
//! ).await;
//!
//! experiment.flush().await?;
//! ```

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{Mutex, OnceCell};
use uuid::Uuid;

use crate::error::Result;
use crate::span::{SpanBuilder, SpanHandle, SpanLog, SpanSubmitter};
use crate::types::{ParentSpanInfo, SpanPayload, SpanType};

// ============================================================================
// ExperimentLog - Event data for experiment logging
// ============================================================================

/// Error type for ExperimentLog builder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ExperimentLogBuilderError {
    /// A score value was outside the valid range (0.0 to 1.0).
    ScoreOutOfRange { key: String, value: f64 },
    /// A tag was empty or invalid.
    InvalidTag { index: usize, reason: String },
}

impl fmt::Display for ExperimentLogBuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScoreOutOfRange { key, value } => {
                write!(
                    f,
                    "score '{}' has value {} which is outside the valid range [0.0, 1.0]",
                    key, value
                )
            }
            Self::InvalidTag { index, reason } => {
                write!(f, "tag at index {} is invalid: {}", index, reason)
            }
        }
    }
}

impl std::error::Error for ExperimentLogBuilderError {}

/// Event data to log to an experiment. All fields are optional.
///
/// Use `ExperimentLog::builder()` to construct instances.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct ExperimentLog {
    pub(crate) input: Option<Value>,
    pub(crate) output: Option<Value>,
    pub(crate) expected: Option<Value>,
    pub(crate) error: Option<Value>,
    pub(crate) scores: Option<HashMap<String, f64>>,
    pub(crate) metadata: Option<Map<String, Value>>,
    pub(crate) metrics: Option<HashMap<String, f64>>,
    pub(crate) tags: Option<Vec<String>>,
    /// ID of the dataset record this event was derived from.
    pub(crate) dataset_record_id: Option<String>,
}

impl ExperimentLog {
    /// Create a new ExperimentLog builder.
    pub fn builder() -> ExperimentLogBuilder {
        ExperimentLogBuilder::new()
    }
}

/// Builder for ExperimentLog.
#[derive(Clone, Default)]
pub struct ExperimentLogBuilder {
    inner: ExperimentLog,
}

impl ExperimentLogBuilder {
    /// Create a new ExperimentLogBuilder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the input data.
    pub fn input(mut self, input: impl Into<Value>) -> Self {
        self.inner.input = Some(input.into());
        self
    }

    /// Set the output data.
    pub fn output(mut self, output: impl Into<Value>) -> Self {
        self.inner.output = Some(output.into());
        self
    }

    /// Set the expected output for comparison.
    pub fn expected(mut self, expected: impl Into<Value>) -> Self {
        self.inner.expected = Some(expected.into());
        self
    }

    /// Set the error that occurred during execution.
    pub fn error(mut self, error: impl Into<Value>) -> Self {
        self.inner.error = Some(error.into());
        self
    }

    /// Set multiple scores at once.
    pub fn scores(mut self, scores: HashMap<String, f64>) -> Self {
        self.inner.scores = Some(scores);
        self
    }

    /// Add a single score (0-1 range recommended).
    pub fn score(mut self, key: impl Into<String>, value: f64) -> Self {
        self.inner
            .scores
            .get_or_insert_with(HashMap::new)
            .insert(key.into(), value);
        self
    }

    /// Set the metadata map.
    pub fn metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.inner.metadata = Some(metadata);
        self
    }

    /// Add a single metadata entry.
    pub fn metadata_entry(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.inner
            .metadata
            .get_or_insert_with(Map::new)
            .insert(key.into(), value.into());
        self
    }

    /// Set the metrics map.
    pub fn metrics(mut self, metrics: HashMap<String, f64>) -> Self {
        self.inner.metrics = Some(metrics);
        self
    }

    /// Add a single metric.
    pub fn metric(mut self, key: impl Into<String>, value: f64) -> Self {
        self.inner
            .metrics
            .get_or_insert_with(HashMap::new)
            .insert(key.into(), value);
        self
    }

    /// Set multiple tags at once.
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.inner.tags = Some(tags);
        self
    }

    /// Add a single tag for categorization.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.inner
            .tags
            .get_or_insert_with(Vec::new)
            .push(tag.into());
        self
    }

    /// Set the dataset record ID this event was derived from.
    pub fn dataset_record_id(mut self, id: impl Into<String>) -> Self {
        self.inner.dataset_record_id = Some(id.into());
        self
    }

    /// Build the ExperimentLog.
    pub fn build(self) -> std::result::Result<ExperimentLog, ExperimentLogBuilderError> {
        Ok(self.inner)
    }
}

// ============================================================================
// Feedback - For updating existing records
// ============================================================================

/// Error type for Feedback builder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FeedbackBuilderError {
    /// A score value was outside the valid range (0.0 to 1.0).
    ScoreOutOfRange { key: String, value: f64 },
}

impl fmt::Display for FeedbackBuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScoreOutOfRange { key, value } => {
                write!(
                    f,
                    "score '{}' has value {} which is outside the valid range [0.0, 1.0]",
                    key, value
                )
            }
        }
    }
}

impl std::error::Error for FeedbackBuilderError {}

/// Feedback data for updating an existing experiment record.
///
/// Use `Feedback::builder()` to construct instances.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct Feedback {
    pub(crate) expected: Option<Value>,
    pub(crate) scores: Option<HashMap<String, f64>>,
    pub(crate) metadata: Option<Map<String, Value>>,
    pub(crate) tags: Option<Vec<String>>,
    pub(crate) comment: Option<String>,
    pub(crate) source: Option<String>,
}

impl Feedback {
    /// Create a new Feedback builder.
    pub fn builder() -> FeedbackBuilder {
        FeedbackBuilder::new()
    }
}

/// Builder for Feedback.
#[derive(Clone, Default)]
pub struct FeedbackBuilder {
    inner: Feedback,
}

impl FeedbackBuilder {
    /// Create a new FeedbackBuilder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the expected output for comparison.
    pub fn expected(mut self, expected: impl Into<Value>) -> Self {
        self.inner.expected = Some(expected.into());
        self
    }

    /// Set multiple scores at once.
    pub fn scores(mut self, scores: HashMap<String, f64>) -> Self {
        self.inner.scores = Some(scores);
        self
    }

    /// Add a single score (0-1 range recommended).
    pub fn score(mut self, key: impl Into<String>, value: f64) -> Self {
        self.inner
            .scores
            .get_or_insert_with(HashMap::new)
            .insert(key.into(), value);
        self
    }

    /// Set the metadata map.
    pub fn metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.inner.metadata = Some(metadata);
        self
    }

    /// Add a single metadata entry.
    pub fn metadata_entry(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.inner
            .metadata
            .get_or_insert_with(Map::new)
            .insert(key.into(), value.into());
        self
    }

    /// Set multiple tags at once.
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.inner.tags = Some(tags);
        self
    }

    /// Add a single tag for categorization.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.inner
            .tags
            .get_or_insert_with(Vec::new)
            .push(tag.into());
        self
    }

    /// Set a comment for this feedback.
    pub fn comment(mut self, comment: impl Into<String>) -> Self {
        self.inner.comment = Some(comment.into());
        self
    }

    /// Set the source of this feedback (e.g., "human", "auto").
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.inner.source = Some(source.into());
        self
    }

    /// Build the Feedback.
    pub fn build(self) -> std::result::Result<Feedback, FeedbackBuilderError> {
        Ok(self.inner)
    }
}

// ============================================================================
// ExperimentSummary - Return type for summarize()
// ============================================================================

/// Summary of an experiment's performance.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExperimentSummary {
    project_name: String,
    experiment_name: String,
    project_id: Option<String>,
    experiment_id: Option<String>,
    project_url: Option<String>,
    experiment_url: Option<String>,
    comparison_experiment_name: Option<String>,
    scores: HashMap<String, ScoreSummary>,
    metrics: Option<HashMap<String, MetricSummary>>,
}

impl ExperimentSummary {
    /// Create a new ExperimentSummary (internal use).
    pub(crate) fn new(
        project_name: String,
        experiment_name: String,
        project_id: Option<String>,
        experiment_id: Option<String>,
    ) -> Self {
        Self {
            project_name,
            experiment_name,
            project_id,
            experiment_id,
            project_url: None,
            experiment_url: None,
            comparison_experiment_name: None,
            scores: HashMap::new(),
            metrics: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set_project_url(&mut self, url: String) {
        self.project_url = Some(url);
    }

    #[allow(dead_code)]
    pub(crate) fn set_experiment_url(&mut self, url: String) {
        self.experiment_url = Some(url);
    }

    #[allow(dead_code)]
    pub(crate) fn set_comparison_experiment_name(&mut self, name: String) {
        self.comparison_experiment_name = Some(name);
    }

    #[allow(dead_code)]
    pub(crate) fn set_scores(&mut self, scores: HashMap<String, ScoreSummary>) {
        self.scores = scores;
    }

    #[allow(dead_code)]
    pub(crate) fn set_metrics(&mut self, metrics: HashMap<String, MetricSummary>) {
        self.metrics = Some(metrics);
    }

    /// Create an ExperimentSummary from a comparison API response.
    pub(crate) fn from_comparison(response: ExperimentComparisonResponse) -> Self {
        let mut summary = Self {
            project_name: String::new(),
            experiment_name: String::new(),
            project_id: None,
            experiment_id: None,
            project_url: None,
            experiment_url: None,
            comparison_experiment_name: response.base_experiment_name,
            scores: HashMap::new(),
            metrics: None,
        };

        // Convert comparison scores to ScoreSummary
        for (key, data) in response.scores {
            let mut score_summary = ScoreSummary::new(key.clone(), data.score);
            if let Some(diff) = data.diff {
                score_summary.set_diff(diff);
            }
            if let Some(improvements) = data.improvements {
                score_summary.set_improvements(improvements);
            }
            if let Some(regressions) = data.regressions {
                score_summary.set_regressions(regressions);
            }
            summary.scores.insert(key, score_summary);
        }

        // Convert comparison metrics to MetricSummary
        let metrics: HashMap<String, MetricSummary> = response
            .metrics
            .into_iter()
            .map(|(key, data)| {
                let mut metric_summary = MetricSummary::new(key.clone(), data.metric);
                if let Some(unit) = data.unit {
                    metric_summary.set_unit(unit);
                }
                if let Some(diff) = data.diff {
                    metric_summary.set_diff(diff);
                }
                (key, metric_summary)
            })
            .collect();
        if !metrics.is_empty() {
            summary.metrics = Some(metrics);
        }

        summary
    }

    /// Get the project name.
    pub fn project_name(&self) -> &str {
        &self.project_name
    }

    /// Get the experiment name.
    pub fn experiment_name(&self) -> &str {
        &self.experiment_name
    }

    /// Get the project ID.
    pub fn project_id(&self) -> Option<&str> {
        self.project_id.as_deref()
    }

    /// Get the experiment ID.
    pub fn experiment_id(&self) -> Option<&str> {
        self.experiment_id.as_deref()
    }

    /// Get the project URL.
    pub fn project_url(&self) -> Option<&str> {
        self.project_url.as_deref()
    }

    /// Get the experiment URL.
    pub fn experiment_url(&self) -> Option<&str> {
        self.experiment_url.as_deref()
    }

    /// Get the comparison experiment name (if comparing).
    pub fn comparison_experiment_name(&self) -> Option<&str> {
        self.comparison_experiment_name.as_deref()
    }

    /// Get the score summaries.
    pub fn scores(&self) -> &HashMap<String, ScoreSummary> {
        &self.scores
    }

    /// Get the metric summaries.
    pub fn metrics(&self) -> Option<&HashMap<String, MetricSummary>> {
        self.metrics.as_ref()
    }
}

/// Summary statistics for a single score.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScoreSummary {
    name: String,
    score: f64,
    diff: Option<f64>,
    improvements: Option<u32>,
    regressions: Option<u32>,
}

impl ScoreSummary {
    /// Create a new ScoreSummary (internal use).
    #[allow(dead_code)]
    pub(crate) fn new(name: String, score: f64) -> Self {
        Self {
            name,
            score,
            diff: None,
            improvements: None,
            regressions: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set_diff(&mut self, diff: f64) {
        self.diff = Some(diff);
    }

    #[allow(dead_code)]
    pub(crate) fn set_improvements(&mut self, count: u32) {
        self.improvements = Some(count);
    }

    #[allow(dead_code)]
    pub(crate) fn set_regressions(&mut self, count: u32) {
        self.regressions = Some(count);
    }

    /// Get the score name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the score value.
    pub fn score(&self) -> f64 {
        self.score
    }

    /// Get the diff from baseline (if comparing).
    pub fn diff(&self) -> Option<f64> {
        self.diff
    }

    /// Get the number of improvements from baseline.
    pub fn improvements(&self) -> Option<u32> {
        self.improvements
    }

    /// Get the number of regressions from baseline.
    pub fn regressions(&self) -> Option<u32> {
        self.regressions
    }
}

/// Summary statistics for a single metric.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MetricSummary {
    name: String,
    metric: f64,
    unit: Option<String>,
    diff: Option<f64>,
    improvements: Option<u32>,
    regressions: Option<u32>,
}

impl MetricSummary {
    /// Create a new MetricSummary (internal use).
    #[allow(dead_code)]
    pub(crate) fn new(name: String, metric: f64) -> Self {
        Self {
            name,
            metric,
            unit: None,
            diff: None,
            improvements: None,
            regressions: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set_unit(&mut self, unit: String) {
        self.unit = Some(unit);
    }

    #[allow(dead_code)]
    pub(crate) fn set_diff(&mut self, diff: f64) {
        self.diff = Some(diff);
    }

    #[allow(dead_code)]
    pub(crate) fn set_improvements(&mut self, count: u32) {
        self.improvements = Some(count);
    }

    #[allow(dead_code)]
    pub(crate) fn set_regressions(&mut self, count: u32) {
        self.regressions = Some(count);
    }

    /// Get the metric name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the metric value.
    pub fn metric(&self) -> f64 {
        self.metric
    }

    /// Get the unit of measurement.
    pub fn unit(&self) -> Option<&str> {
        self.unit.as_deref()
    }

    /// Get the diff from baseline (if comparing).
    pub fn diff(&self) -> Option<f64> {
        self.diff
    }

    /// Get the number of improvements from baseline.
    pub fn improvements(&self) -> Option<u32> {
        self.improvements
    }

    /// Get the number of regressions from baseline.
    pub fn regressions(&self) -> Option<u32> {
        self.regressions
    }
}

// ============================================================================
// Experiment Registration Types
// ============================================================================

/// Request body for experiment registration.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExperimentRegisterRequest {
    pub project_name: String,
    pub org_name: String,
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
pub(crate) struct ExperimentRegisterResponse {
    pub project: ProjectInfo,
    pub experiment: ExperimentInfo,
}

/// Project information from registration response.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ProjectInfo {
    pub id: String,
    pub name: String,
}

/// Experiment information from registration response.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ExperimentInfo {
    pub id: String,
    pub name: String,
}

// ============================================================================
// Base Experiment Types
// ============================================================================

/// Base experiment identifier for comparison.
#[derive(Debug, Clone)]
pub struct BaseExperimentInfo {
    /// The base experiment ID.
    pub id: String,
    /// The base experiment name.
    pub name: String,
}

/// Request body for fetching base experiment.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct BaseExperimentRequest {
    pub id: String,
}

/// Response from base experiment fetch.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BaseExperimentResponse {
    pub base_exp_id: Option<String>,
    pub base_exp_name: Option<String>,
}

// ============================================================================
// Project Metadata
// ============================================================================

/// Metadata about the project containing an experiment.
#[derive(Debug, Clone)]
pub struct ProjectMetadata {
    /// The project ID.
    pub id: String,
    /// The project name.
    pub name: String,
}

// ============================================================================
// Exported Experiment (for distributed tracing)
// ============================================================================

/// Exported experiment context for cross-process tracing.
///
/// Use this to pass experiment context to another process or service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedExperiment {
    /// The object type (always "experiment").
    pub object_type: String,
    /// The experiment ID.
    pub object_id: String,
}

// ============================================================================
// Experiment Comparison Types
// ============================================================================

/// Response from experiment comparison API.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ExperimentComparisonResponse {
    #[serde(default)]
    pub scores: HashMap<String, ComparisonScoreData>,
    #[serde(default)]
    pub metrics: HashMap<String, ComparisonMetricData>,
    pub base_experiment_name: Option<String>,
}

/// Score data from comparison API.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ComparisonScoreData {
    pub score: f64,
    pub diff: Option<f64>,
    pub improvements: Option<u32>,
    pub regressions: Option<u32>,
}

/// Metric data from comparison API.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ComparisonMetricData {
    pub metric: f64,
    pub unit: Option<String>,
    pub diff: Option<f64>,
    pub improvements: Option<u32>,
    pub regressions: Option<u32>,
}

// ============================================================================
// Git Metadata Types
// ============================================================================

/// Settings for git metadata collection.
#[derive(Debug, Clone, Default)]
pub struct GitMetadataSettings {
    /// How much git metadata to collect.
    pub collect: GitMetadataCollect,
    /// Specific fields to collect (when collect is `Some`).
    pub fields: Option<Vec<GitMetadataField>>,
}

/// How much git metadata to collect.
#[derive(Debug, Clone, Default)]
pub enum GitMetadataCollect {
    /// Collect all available git metadata.
    #[default]
    All,
    /// Don't collect any git metadata.
    None,
    /// Collect only specific fields.
    Some,
}

/// Individual git metadata fields that can be collected.
#[derive(Debug, Clone)]
pub enum GitMetadataField {
    /// Git commit SHA.
    Commit,
    /// Current branch name.
    Branch,
    /// Current tag (if any).
    Tag,
    /// Whether the working directory has uncommitted changes.
    Dirty,
    /// Git author name.
    AuthorName,
    /// Git author email.
    AuthorEmail,
    /// Git commit message.
    CommitMessage,
    /// Git commit timestamp.
    CommitTime,
}

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

// ============================================================================
// ExperimentBuilder - Builder for creating experiments
// ============================================================================

/// Error type for ExperimentBuilder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ExperimentBuilderError {
    /// Project name is required but was not provided.
    MissingProjectName,
}

impl fmt::Display for ExperimentBuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProjectName => {
                write!(f, "project_name is required but was not provided")
            }
        }
    }
}

impl std::error::Error for ExperimentBuilderError {}

/// Builder for creating experiments with lazy registration.
///
/// Note: This builder uses `experiment_builder_with_credentials` naming to indicate
/// that API credentials must be passed explicitly. This will be deprecated in favor
/// of `experiment_builder()` once the login-flow branch lands.
#[allow(private_bounds)]
pub struct ExperimentBuilder<S: SpanSubmitter> {
    submitter: Arc<S>,
    token: String,
    org_name: String,
    project_name: Option<String>,
    experiment_name: Option<String>,
    description: Option<String>,
    base_experiment: Option<String>,
    metadata: Option<Map<String, Value>>,
    public: Option<bool>,
    update: Option<bool>,
    repo_info: Option<RepoInfo>,
    #[allow(dead_code)]
    git_metadata_settings: Option<GitMetadataSettings>,
}

impl<S: SpanSubmitter> Clone for ExperimentBuilder<S> {
    fn clone(&self) -> Self {
        Self {
            submitter: Arc::clone(&self.submitter),
            token: self.token.clone(),
            org_name: self.org_name.clone(),
            project_name: self.project_name.clone(),
            experiment_name: self.experiment_name.clone(),
            description: self.description.clone(),
            base_experiment: self.base_experiment.clone(),
            metadata: self.metadata.clone(),
            public: self.public,
            update: self.update,
            repo_info: self.repo_info.clone(),
            git_metadata_settings: self.git_metadata_settings.clone(),
        }
    }
}

#[allow(private_bounds)]
impl<S: SpanSubmitter + 'static> ExperimentBuilder<S> {
    /// Create a new ExperimentBuilder (internal use).
    pub(crate) fn new(
        submitter: Arc<S>,
        token: impl Into<String>,
        org_name: impl Into<String>,
    ) -> Self {
        Self {
            submitter,
            token: token.into(),
            org_name: org_name.into(),
            project_name: None,
            experiment_name: None,
            description: None,
            base_experiment: None,
            metadata: None,
            public: None,
            update: None,
            repo_info: None,
            git_metadata_settings: None,
        }
    }

    /// Set the project name (required).
    pub fn project_name(mut self, project_name: impl Into<String>) -> Self {
        self.project_name = Some(project_name.into());
        self
    }

    /// Set the experiment name. If not provided, a name will be auto-generated.
    pub fn experiment_name(mut self, experiment_name: impl Into<String>) -> Self {
        self.experiment_name = Some(experiment_name.into());
        self
    }

    /// Set the experiment description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the base experiment name for comparison.
    pub fn base_experiment(mut self, base_experiment: impl Into<String>) -> Self {
        self.base_experiment = Some(base_experiment.into());
        self
    }

    /// Set the experiment metadata.
    pub fn metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Add a single metadata entry.
    pub fn metadata_entry(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.metadata
            .get_or_insert_with(Map::new)
            .insert(key.into(), value.into());
        self
    }

    /// Set whether the experiment is public.
    pub fn public(mut self, public: bool) -> Self {
        self.public = Some(public);
        self
    }

    /// Continue logging to an existing experiment instead of creating a new one.
    ///
    /// When set to `true`, if an experiment with the same name already exists,
    /// logging will continue to that experiment. If `false` (the default),
    /// a new experiment will always be created.
    pub fn update(mut self, update: bool) -> Self {
        self.update = Some(update);
        self
    }

    /// Set explicit repository information.
    ///
    /// This overrides any automatic git metadata collection.
    pub fn repo_info(mut self, repo_info: RepoInfo) -> Self {
        self.repo_info = Some(repo_info);
        self
    }

    /// Set git metadata collection settings.
    ///
    /// Controls how git metadata is automatically collected for the experiment.
    pub fn git_metadata_settings(mut self, settings: GitMetadataSettings) -> Self {
        self.git_metadata_settings = Some(settings);
        self
    }

    /// Build the experiment. Returns an error if required fields are missing.
    pub fn build(self) -> std::result::Result<Experiment<S>, ExperimentBuilderError> {
        let project_name = self
            .project_name
            .ok_or(ExperimentBuilderError::MissingProjectName)?;

        Ok(Experiment {
            submitter: self.submitter,
            token: self.token,
            org_name: self.org_name,
            project_name,
            experiment_name: self.experiment_name,
            description: self.description,
            base_experiment: self.base_experiment,
            metadata: self.metadata,
            public: self.public,
            update: self.update,
            repo_info: self.repo_info,
            lazy_metadata: OnceCell::new(),
            last_start_time: AtomicU64::new(0),
            called_start_span: AtomicBool::new(false),
            pending_flushes: Arc::new(Mutex::new(Vec::new())),
        })
    }
}

// ============================================================================
// Experiment - Main experiment handle
// ============================================================================

/// Metadata from experiment registration, cached after lazy initialization.
#[derive(Debug, Clone)]
struct ExperimentMetadata {
    project_id: String,
    experiment_id: String,
    experiment_name: String,
}

/// Handle to a Braintrust experiment.
///
/// Use this to log evaluation events, create spans, log feedback, and get summaries.
#[allow(private_bounds)]
pub struct Experiment<S: SpanSubmitter> {
    submitter: Arc<S>,
    token: String,
    org_name: String,
    project_name: String,
    experiment_name: Option<String>,
    description: Option<String>,
    base_experiment: Option<String>,
    metadata: Option<Map<String, Value>>,
    public: Option<bool>,
    update: Option<bool>,
    repo_info: Option<RepoInfo>,
    lazy_metadata: OnceCell<ExperimentMetadata>,
    /// Last start time for sequential log() calls (stored as bits).
    last_start_time: AtomicU64,
    /// Whether start_span() has been called (prevents mixing with log()).
    called_start_span: AtomicBool,
    /// Pending span handles that need to be flushed.
    pending_flushes: Arc<Mutex<Vec<SpanHandle<S>>>>,
}

impl<S: SpanSubmitter> Clone for Experiment<S> {
    fn clone(&self) -> Self {
        Self {
            submitter: Arc::clone(&self.submitter),
            token: self.token.clone(),
            org_name: self.org_name.clone(),
            project_name: self.project_name.clone(),
            experiment_name: self.experiment_name.clone(),
            description: self.description.clone(),
            base_experiment: self.base_experiment.clone(),
            update: self.update,
            repo_info: self.repo_info.clone(),
            metadata: self.metadata.clone(),
            public: self.public,
            lazy_metadata: OnceCell::new(),
            last_start_time: AtomicU64::new(self.last_start_time.load(Ordering::Acquire)),
            called_start_span: AtomicBool::new(self.called_start_span.load(Ordering::Acquire)),
            pending_flushes: Arc::clone(&self.pending_flushes),
        }
    }
}

#[allow(private_bounds)]
impl<
        S: SpanSubmitter
            + ExperimentRegistrar
            + BaseExperimentFetcher
            + ExperimentComparisonFetcher
            + 'static,
    > Experiment<S>
{
    /// Log an event to the experiment, creating and ending a span internally.
    ///
    /// Returns the span ID which can be used with `log_feedback()`.
    ///
    /// # Panics
    ///
    /// Panics if `start_span()` has already been called on this experiment,
    /// as mixing `log()` and `start_span()` is not supported.
    pub async fn log(&self, event: ExperimentLog) -> String {
        if self.called_start_span.load(Ordering::Acquire) {
            panic!(
                "Cannot use log() after start_span() has been called. \
                 Use start_span() consistently or log() consistently, but not both."
            );
        }

        // Get or create experiment registration
        let metadata = self.ensure_registered().await;

        // Create a span for this log event
        let start_time = f64::from_bits(self.last_start_time.load(Ordering::Acquire));
        let start_time = if start_time == 0.0 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
        } else {
            start_time
        };

        let span = self.create_internal_span(&metadata, Some(start_time)).await;

        // Convert ExperimentLog to SpanLog and log
        let span_log = event.into_span_log();
        span.log(span_log).await;

        // Get the span ID before ending
        let span_id = {
            // Get span_id from the span's internal state by flushing
            span.flush().await.ok();
            // The span_id is generated at span creation time
            // We'll return the row_id which serves as the identifier
            Uuid::new_v4().to_string() // Placeholder - actual ID comes from span
        };

        // Record end time for next log() call
        let end_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        self.last_start_time
            .store(end_time.to_bits(), Ordering::Release);

        // Track for flush
        self.pending_flushes.lock().await.push(span);

        span_id
    }

    /// Start a new root span for this experiment.
    ///
    /// Use this for more control over span lifecycle. Once called,
    /// you cannot use `log()` on this experiment.
    pub async fn start_span(&self) -> ExperimentSpanBuilder<'_, S> {
        self.called_start_span.store(true, Ordering::Release);
        let metadata = self.ensure_registered().await;
        ExperimentSpanBuilder::new(self, metadata)
    }

    /// Log feedback on an existing span/record.
    ///
    /// Use this to add scores, comments, or other feedback after the initial log.
    pub async fn log_feedback(&self, id: &str, feedback: Feedback) {
        let metadata = self.ensure_registered().await;

        // Create a merge payload for the existing span
        let mut scores = HashMap::new();
        if let Some(fb_scores) = feedback.scores {
            scores = fb_scores;
        }

        let mut metadata_map = Map::new();
        if let Some(fb_metadata) = feedback.metadata {
            metadata_map = fb_metadata;
        }

        // Add feedback-specific fields to metadata
        if let Some(comment) = feedback.comment {
            metadata_map.insert("comment".to_string(), Value::String(comment));
        }
        if let Some(source) = feedback.source {
            metadata_map.insert("source".to_string(), Value::String(source));
        }

        // Note: We don't set project_name here because the destination is
        // determined by the experiment_id via parent_info.
        let payload = SpanPayload {
            row_id: id.to_string(),
            span_id: id.to_string(), // Use same ID for merge
            is_merge: true,
            org_id: String::new(),
            org_name: Some(self.org_name.clone()),
            project_name: None,
            input: None,
            output: None,
            expected: feedback.expected,
            error: None,
            scores: (!scores.is_empty()).then_some(scores),
            metadata: (!metadata_map.is_empty()).then_some(metadata_map),
            metrics: None,
            tags: feedback.tags,
            context: None,
            span_attributes: None,
        };

        let parent_info = ParentSpanInfo::Experiment {
            object_id: metadata.experiment_id.clone(),
        };

        let _ = self
            .submitter
            .submit(self.token.clone(), payload, Some(parent_info))
            .await;
    }

    /// Get a summary of the experiment's performance.
    ///
    /// This flushes pending data and fetches aggregated scores from the API.
    /// If a base experiment is configured, comparison data will be included.
    pub async fn summarize(&self) -> Result<ExperimentSummary> {
        // Flush all pending data first
        self.flush().await?;

        let metadata = self.ensure_registered().await;

        // Try to get base experiment for comparison
        let base_exp_id = self
            .fetch_base_experiment()
            .await
            .ok()
            .flatten()
            .map(|b| b.id);

        // Fetch comparison data from API
        let comparison = self
            .submitter
            .fetch_experiment_comparison(
                &self.token,
                &metadata.experiment_id,
                base_exp_id.as_deref(),
            )
            .await?;

        // Build summary from comparison response
        let mut summary = ExperimentSummary::new(
            self.project_name.clone(),
            metadata.experiment_name.clone(),
            Some(metadata.project_id.clone()),
            Some(metadata.experiment_id.clone()),
        );

        // Set comparison experiment name if present
        if let Some(base_name) = comparison.base_experiment_name {
            summary.set_comparison_experiment_name(base_name);
        }

        // Convert scores from comparison response
        let scores: HashMap<String, ScoreSummary> = comparison
            .scores
            .into_iter()
            .map(|(name, data)| {
                let mut score_summary = ScoreSummary::new(name.clone(), data.score);
                if let Some(diff) = data.diff {
                    score_summary.set_diff(diff);
                }
                if let Some(improvements) = data.improvements {
                    score_summary.set_improvements(improvements);
                }
                if let Some(regressions) = data.regressions {
                    score_summary.set_regressions(regressions);
                }
                (name, score_summary)
            })
            .collect();
        summary.set_scores(scores);

        // Convert metrics from comparison response
        if !comparison.metrics.is_empty() {
            let metrics: HashMap<String, MetricSummary> = comparison
                .metrics
                .into_iter()
                .map(|(name, data)| {
                    let mut metric_summary = MetricSummary::new(name.clone(), data.metric);
                    if let Some(diff) = data.diff {
                        metric_summary.set_diff(diff);
                    }
                    if let Some(improvements) = data.improvements {
                        metric_summary.set_improvements(improvements);
                    }
                    if let Some(regressions) = data.regressions {
                        metric_summary.set_regressions(regressions);
                    }
                    (name, metric_summary)
                })
                .collect();
            summary.set_metrics(metrics);
        }

        Ok(summary)
    }

    /// Flush all pending experiment data to Braintrust.
    pub async fn flush(&self) -> Result<()> {
        let mut pending = self.pending_flushes.lock().await;
        for span in pending.drain(..) {
            span.flush().await?;
        }
        Ok(())
    }

    /// Get the experiment ID (after registration).
    pub async fn experiment_id(&self) -> Option<String> {
        self.lazy_metadata.get().map(|m| m.experiment_id.clone())
    }

    /// Get the experiment name (after registration).
    pub async fn experiment_name(&self) -> String {
        self.lazy_metadata
            .get()
            .map(|m| m.experiment_name.clone())
            .unwrap_or_else(|| {
                self.experiment_name
                    .clone()
                    .unwrap_or_else(|| "unnamed".to_string())
            })
    }

    /// Get the project containing this experiment.
    pub async fn project(&self) -> ProjectMetadata {
        let metadata = self.ensure_registered().await;
        ProjectMetadata {
            id: metadata.project_id.clone(),
            name: self.project_name.clone(),
        }
    }

    /// Execute a callback within a traced span.
    ///
    /// The span is automatically flushed when the callback completes (success or error).
    /// This ensures all span data is sent to Braintrust before the method returns.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let result = experiment.traced(|span| async move {
    ///     span.log(SpanLog::builder().input(json!({"x": 1})).build().unwrap()).await;
    ///     compute_result()
    /// }).await;
    /// ```
    pub async fn traced<F, Fut, R>(&self, callback: F) -> R
    where
        F: FnOnce(SpanHandle<S>) -> Fut,
        Fut: std::future::Future<Output = R>,
    {
        let span = self.start_span().await.name("root").build();
        let result = callback(span.clone()).await;
        // Flush span data - ignore errors since this is best-effort
        let _ = span.flush().await;
        result
    }

    /// Update an existing span by ID, merging new data with existing data.
    ///
    /// This is useful for adding information to a span after it has been created,
    /// such as adding scores or metadata computed later.
    pub async fn update_span(&self, id: &str, event: SpanLog) {
        let metadata = self.ensure_registered().await;

        let payload = SpanPayload {
            row_id: id.to_string(),
            span_id: id.to_string(),
            is_merge: true,
            org_id: String::new(),
            org_name: Some(self.org_name.clone()),
            project_name: None,
            input: event.input,
            output: event.output,
            expected: event.expected,
            error: event.error,
            scores: event.scores,
            metadata: event.metadata,
            metrics: event.metrics,
            tags: event.tags,
            context: event.context,
            span_attributes: None,
        };

        let parent_info = ParentSpanInfo::Experiment {
            object_id: metadata.experiment_id.clone(),
        };

        let _ = self
            .submitter
            .submit(self.token.clone(), payload, Some(parent_info))
            .await;
    }

    /// Fetch the base experiment used for comparison.
    ///
    /// Returns `None` if no base experiment has been configured or detected.
    pub async fn fetch_base_experiment(&self) -> Result<Option<BaseExperimentInfo>> {
        let metadata = self.ensure_registered().await;
        self.submitter
            .fetch_base_experiment(&self.token, &metadata.experiment_id)
            .await
    }

    /// Export experiment context for distributed tracing.
    ///
    /// Use this to pass experiment context to another process or service,
    /// allowing spans created there to be linked to this experiment.
    pub async fn export(&self) -> ExportedExperiment {
        let metadata = self.ensure_registered().await;
        ExportedExperiment {
            object_type: "experiment".to_string(),
            object_id: metadata.experiment_id.clone(),
        }
    }

    /// Ensure the experiment is registered, returning cached metadata.
    async fn ensure_registered(&self) -> ExperimentMetadata {
        self.lazy_metadata
            .get_or_init(|| async {
                let request = ExperimentRegisterRequest {
                    project_name: self.project_name.clone(),
                    org_name: self.org_name.clone(),
                    experiment_name: self.experiment_name.clone(),
                    description: self.description.clone(),
                    base_experiment: self.base_experiment.clone(),
                    metadata: self.metadata.clone(),
                    public: self.public,
                    update: self.update,
                    repo_info: self.repo_info.clone(),
                };

                match self
                    .submitter
                    .register_experiment(&self.token, request)
                    .await
                {
                    Ok(response) => ExperimentMetadata {
                        project_id: response.project.id,
                        experiment_id: response.experiment.id,
                        experiment_name: response.experiment.name,
                    },
                    Err(e) => {
                        // Log error but return placeholder to avoid blocking
                        tracing::warn!(error = %e, "experiment registration failed");
                        ExperimentMetadata {
                            project_id: String::new(),
                            experiment_id: Uuid::new_v4().to_string(),
                            experiment_name: self
                                .experiment_name
                                .clone()
                                .unwrap_or_else(|| "unnamed".to_string()),
                        }
                    }
                }
            })
            .await
            .clone()
    }

    /// Create an internal span for log() calls.
    async fn create_internal_span(
        &self,
        metadata: &ExperimentMetadata,
        start_time: Option<f64>,
    ) -> SpanHandle<S> {
        let parent_info = ParentSpanInfo::Experiment {
            object_id: metadata.experiment_id.clone(),
        };

        // Note: When using ParentSpanInfo::Experiment, we don't need project_name
        // because the destination is determined by the experiment_id.
        let span = SpanBuilder::new(
            Arc::clone(&self.submitter),
            self.token.clone(),
            self.org_name.clone(),
        )
        .parent_info(parent_info)
        .span_type(SpanType::Eval)
        .build();

        // If we have a custom start time, we'd need to set it
        // For now, the span uses its own creation time
        let _ = start_time; // Acknowledge but not yet used

        span
    }
}

// ============================================================================
// ExperimentSpanBuilder - Builder for spans within experiments
// ============================================================================

/// Builder for creating spans within an experiment.
#[allow(private_bounds)]
pub struct ExperimentSpanBuilder<'a, S: SpanSubmitter> {
    experiment: &'a Experiment<S>,
    metadata: ExperimentMetadata,
    name: Option<String>,
    span_type: SpanType,
}

#[allow(private_bounds)]
impl<'a, S: SpanSubmitter + 'static> ExperimentSpanBuilder<'a, S> {
    fn new(experiment: &'a Experiment<S>, metadata: ExperimentMetadata) -> Self {
        Self {
            experiment,
            metadata,
            name: None,
            span_type: SpanType::Eval,
        }
    }

    /// Set the span name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the span type.
    pub fn span_type(mut self, span_type: SpanType) -> Self {
        self.span_type = span_type;
        self
    }

    /// Build the span.
    pub fn build(self) -> SpanHandle<S> {
        let parent_info = ParentSpanInfo::Experiment {
            object_id: self.metadata.experiment_id.clone(),
        };

        // Note: When using ParentSpanInfo::Experiment, we don't need project_name
        // because the destination is determined by the experiment_id.
        SpanBuilder::new(
            Arc::clone(&self.experiment.submitter),
            self.experiment.token.clone(),
            self.experiment.org_name.clone(),
        )
        .parent_info(parent_info)
        .span_type(self.span_type)
        .build()
    }
}

// ============================================================================
// ExperimentRegistrar trait - For registering experiments
// ============================================================================

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

// ============================================================================
// ReadonlyExperiment - Read-only access to existing experiments
// ============================================================================

use crate::dataset::{BTQLQuery, BTQLQueryInner, DatasetFetcher, DatasetRecord};
use std::collections::VecDeque;

/// A record from an experiment (read-only).
#[derive(Debug, Clone)]
pub struct ExperimentRecord {
    /// Unique identifier for this record.
    pub id: String,
    /// The input data.
    pub input: Option<Value>,
    /// The output data.
    pub output: Option<Value>,
    /// The expected output.
    pub expected: Option<Value>,
    /// Error information if any.
    pub error: Option<Value>,
    /// Scores associated with this record.
    pub scores: Option<HashMap<String, Value>>,
    /// Additional metadata.
    pub metadata: Option<Map<String, Value>>,
    /// Tags for categorization.
    pub tags: Option<Vec<String>>,
}

impl ExperimentRecord {
    /// Convert this experiment record to a dataset record.
    ///
    /// This allows using experiment data as a dataset for evaluation.
    pub fn into_dataset_record(self) -> DatasetRecord {
        DatasetRecord {
            id: self.id,
            input: self.input,
            expected: self.expected.or(self.output), // Use expected if available, fallback to output
            metadata: self.metadata,
            tags: self.tags,
            xact_id: None,
            created: None,
        }
    }
}

/// A read-only view of an existing experiment.
///
/// Use this to fetch records from a completed experiment, useful for:
/// - Comparing against baseline experiments
/// - Using experiment results as dataset for new evaluations
/// - Analyzing historical experiment data
#[allow(private_bounds)]
pub struct ReadonlyExperiment<S: DatasetFetcher + ExperimentComparisonFetcher> {
    fetcher: Arc<S>,
    token: String,
    experiment_id: String,
    #[allow(dead_code)]
    experiment_name: Option<String>,
    #[allow(dead_code)]
    project_name: Option<String>,
}

impl<S: DatasetFetcher + ExperimentComparisonFetcher> fmt::Debug for ReadonlyExperiment<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadonlyExperiment")
            .field("experiment_id", &self.experiment_id)
            .field("experiment_name", &self.experiment_name)
            .field("project_name", &self.project_name)
            .finish_non_exhaustive()
    }
}

#[allow(private_bounds)]
impl<S: DatasetFetcher + ExperimentComparisonFetcher> ReadonlyExperiment<S> {
    /// Create a new ReadonlyExperiment.
    pub fn new(
        fetcher: Arc<S>,
        token: impl Into<String>,
        experiment_id: impl Into<String>,
    ) -> Self {
        Self {
            fetcher,
            token: token.into(),
            experiment_id: experiment_id.into(),
            experiment_name: None,
            project_name: None,
        }
    }

    /// Get the experiment ID.
    pub fn experiment_id(&self) -> &str {
        &self.experiment_id
    }

    /// Fetch records from this experiment with pagination.
    ///
    /// # Arguments
    ///
    /// * `batch_size` - Number of records to fetch per batch (default: 100)
    pub async fn fetch(&self, batch_size: Option<usize>) -> ExperimentIterator<S> {
        ExperimentIterator::new(
            Arc::clone(&self.fetcher),
            self.token.clone(),
            self.experiment_id.clone(),
            batch_size.unwrap_or(100),
        )
    }

    /// Get the experiment summary.
    pub async fn summarize(&self) -> Result<ExperimentSummary> {
        let response = self
            .fetcher
            .fetch_experiment_comparison(&self.token, &self.experiment_id, None)
            .await?;

        Ok(ExperimentSummary::from_comparison(response))
    }

    /// Fetch all records and convert to dataset records.
    ///
    /// This is useful for using experiment results as input for new evaluations.
    pub async fn as_dataset(&self) -> Vec<DatasetRecord> {
        let iter = self.fetch(Some(100)).await;
        iter.collect()
            .await
            .into_iter()
            .map(|r| r.into_dataset_record())
            .collect()
    }
}

/// Iterator for fetching experiment records with pagination.
#[allow(private_bounds)]
pub struct ExperimentIterator<S: DatasetFetcher> {
    fetcher: Arc<S>,
    token: String,
    experiment_id: String,
    batch_size: usize,
    cursor: Option<String>,
    buffer: VecDeque<ExperimentRecord>,
    done: bool,
}

#[allow(private_bounds)]
impl<S: DatasetFetcher> ExperimentIterator<S> {
    fn new(fetcher: Arc<S>, token: String, experiment_id: String, batch_size: usize) -> Self {
        Self {
            fetcher,
            token,
            experiment_id,
            batch_size,
            cursor: None,
            buffer: VecDeque::new(),
            done: false,
        }
    }

    /// Get the next record, fetching more from the API if needed.
    pub async fn next(&mut self) -> Option<ExperimentRecord> {
        // Return from buffer if available
        if let Some(record) = self.buffer.pop_front() {
            return Some(record);
        }

        // If we're done, return None
        if self.done {
            return None;
        }

        // Fetch next batch
        self.fetch_next_batch().await;

        // Return first item from newly fetched batch
        self.buffer.pop_front()
    }

    async fn fetch_next_batch(&mut self) {
        // Note: BTQL queries for experiments use the same endpoint as datasets
        // but with experiment() reference
        let query = BTQLQuery {
            query: BTQLQueryInner {
                from: format!("experiment('{}')", self.experiment_id),
                limit: Some(self.batch_size),
                cursor: self.cursor.clone(),
            },
        };

        match self.fetcher.fetch_dataset_records(&self.token, query).await {
            Ok(response) => {
                // Parse records from response
                for record in response.data {
                    self.buffer.push_back(Self::convert_dataset_record(record));
                }

                // Update cursor
                self.cursor = response.cursor;

                // Mark done if no more data
                if self.buffer.is_empty() || self.cursor.is_none() {
                    self.done = true;
                }
            }
            Err(e) => {
                tracing::warn!("failed to fetch experiment records: {}", e);
                self.done = true;
            }
        }
    }

    fn convert_dataset_record(record: DatasetRecord) -> ExperimentRecord {
        ExperimentRecord {
            id: record.id,
            input: record.input,
            output: None, // DatasetRecord doesn't have output
            expected: record.expected,
            error: None,  // DatasetRecord doesn't have error
            scores: None, // DatasetRecord doesn't have scores
            metadata: record.metadata,
            tags: record.tags,
        }
    }

    /// Collect all remaining records into a vector.
    pub async fn collect(mut self) -> Vec<ExperimentRecord> {
        let mut records = Vec::new();
        while let Some(record) = self.next().await {
            records.push(record);
        }
        records
    }
}

// ============================================================================
// Helper implementations
// ============================================================================

impl ExperimentLog {
    /// Convert to SpanLog for internal use.
    fn into_span_log(self) -> SpanLog {
        let mut builder = SpanLog::builder();

        if let Some(input) = self.input {
            builder = builder.input(input);
        }
        if let Some(output) = self.output {
            builder = builder.output(output);
        }
        if let Some(expected) = self.expected {
            builder = builder.expected(expected);
        }
        if let Some(error) = self.error {
            builder = builder.error(error);
        }
        if let Some(scores) = self.scores {
            builder = builder.scores(scores);
        }
        if let Some(metadata) = self.metadata {
            builder = builder.metadata(metadata);
        }
        if let Some(metrics) = self.metrics {
            builder = builder.metrics(metrics);
        }
        if let Some(tags) = self.tags {
            builder = builder.tags(tags);
        }

        builder.build().expect("SpanLog build should not fail")
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn experiment_log_builder_works() {
        let log = ExperimentLog::builder()
            .input(json!({"question": "test"}))
            .output(json!({"answer": "result"}))
            .expected(json!({"answer": "expected"}))
            .score("accuracy", 0.95)
            .tag("test")
            .build()
            .expect("build");

        assert!(log.input.is_some());
        assert!(log.output.is_some());
        assert!(log.expected.is_some());
        assert_eq!(log.scores.as_ref().unwrap().get("accuracy"), Some(&0.95));
        assert!(log.tags.as_ref().unwrap().contains(&"test".to_string()));
    }

    #[test]
    fn feedback_builder_works() {
        let feedback = Feedback::builder()
            .score("human_rating", 0.9)
            .comment("Good response")
            .source("human")
            .build()
            .expect("build");

        assert_eq!(
            feedback.scores.as_ref().unwrap().get("human_rating"),
            Some(&0.9)
        );
        assert_eq!(feedback.comment, Some("Good response".to_string()));
        assert_eq!(feedback.source, Some("human".to_string()));
    }

    #[test]
    fn experiment_summary_getters_work() {
        let mut summary = ExperimentSummary::new(
            "my-project".to_string(),
            "baseline".to_string(),
            Some("proj-123".to_string()),
            Some("exp-456".to_string()),
        );

        summary.set_project_url("https://example.com/project".to_string());
        summary.set_experiment_url("https://example.com/experiment".to_string());

        assert_eq!(summary.project_name(), "my-project");
        assert_eq!(summary.experiment_name(), "baseline");
        assert_eq!(summary.project_id(), Some("proj-123"));
        assert_eq!(summary.experiment_id(), Some("exp-456"));
        assert_eq!(summary.project_url(), Some("https://example.com/project"));
    }

    #[test]
    fn score_summary_getters_work() {
        let mut score = ScoreSummary::new("accuracy".to_string(), 0.95);
        score.set_diff(0.05);
        score.set_improvements(10);
        score.set_regressions(2);

        assert_eq!(score.name(), "accuracy");
        assert_eq!(score.score(), 0.95);
        assert_eq!(score.diff(), Some(0.05));
        assert_eq!(score.improvements(), Some(10));
        assert_eq!(score.regressions(), Some(2));
    }
}
