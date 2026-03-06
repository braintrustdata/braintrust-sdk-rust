//! Summary types for experiment performance statistics.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

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
