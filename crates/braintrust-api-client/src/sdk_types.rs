//! Types for SDK operations provided by the Braintrust backend.
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ExperimentComparisonResponse {
    #[serde(default)]
    pub scores: HashMap<String, ComparisonScoreData>,
    #[serde(default)]
    pub metrics: HashMap<String, ComparisonMetricData>,
    pub base_experiment_name: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ComparisonScoreData {
    pub score: f64,
    pub diff: Option<f64>,
    pub improvements: Option<u32>,
    pub regressions: Option<u32>,
}
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ComparisonMetricData {
    pub metric: f64,
    pub unit: Option<String>,
    pub diff: Option<f64>,
    pub improvements: Option<u32>,
    pub regressions: Option<u32>,
}
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct DatasetSummaryResponse {
    pub project_name: String,
    pub dataset_name: String,
    pub project_id: String,
    pub dataset_id: String,
    pub project_url: Option<String>,
    pub dataset_url: Option<String>,
    pub data_summary: Option<DataSummaryStats>,
}
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct DataSummaryStats {
    pub total_records: Option<u64>,
}
