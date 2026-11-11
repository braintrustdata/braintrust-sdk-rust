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

/// Repository metadata recorded with an experiment.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[non_exhaustive]
pub struct ExperimentRepoInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_time: Option<String>,
}
/// Experiment registration parameters.
#[derive(Debug, Clone, serde::Serialize)]
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
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_info: Option<ExperimentRepoInfo>,
}
impl ExperimentRegisterRequest {
    pub fn new(project_name: String, org_id: String) -> Self {
        Self {
            project_name,
            org_id,
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
/// Dataset registration parameters.
#[derive(Debug, Clone, serde::Serialize)]
#[non_exhaustive]
pub struct DatasetRegisterRequest {
    pub project_name: String,
    pub org_name: String,
    pub dataset_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}
impl DatasetRegisterRequest {
    pub fn new(project_name: String, org_name: String, dataset_name: String) -> Self {
        Self {
            project_name,
            org_name,
            dataset_name,
            description: None,
            metadata: None,
        }
    }
}
/// A resource returned during registration.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct RegisteredResource {
    pub id: String,
    pub name: String,
}
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ExperimentRegisterResponse {
    pub project: RegisteredResource,
    pub experiment: RegisteredResource,
}
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct DatasetRegisterResponse {
    pub project: RegisteredResource,
    pub dataset: RegisteredResource,
}
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct BaseExperimentResponse {
    pub base_exp_id: Option<String>,
    pub base_exp_name: Option<String>,
}
