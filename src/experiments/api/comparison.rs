//! Internal API types for experiment comparison.

use std::collections::HashMap;

use serde::Deserialize;

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
