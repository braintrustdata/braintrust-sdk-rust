//! Summary types for experiment performance statistics.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Summary of an experiment's performance.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExperimentSummary {
    pub(super) project_name: String,
    pub(super) experiment_name: String,
    pub(super) project_id: Option<String>,
    pub(super) experiment_id: Option<String>,
    pub(super) project_url: Option<String>,
    pub(super) experiment_url: Option<String>,
    pub(super) comparison_experiment_name: Option<String>,
    pub(super) scores: HashMap<String, ScoreSummary>,
    pub(super) metrics: Option<HashMap<String, MetricSummary>>,
}

impl ExperimentSummary {
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
    pub(super) name: String,
    pub(super) score: f64,
    pub(super) diff: Option<f64>,
    pub(super) improvements: Option<u32>,
    pub(super) regressions: Option<u32>,
}

impl ScoreSummary {
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
    pub(super) name: String,
    pub(super) metric: f64,
    pub(super) unit: Option<String>,
    pub(super) diff: Option<f64>,
    pub(super) improvements: Option<u32>,
    pub(super) regressions: Option<u32>,
}

impl MetricSummary {
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
