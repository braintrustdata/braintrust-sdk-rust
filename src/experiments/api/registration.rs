//! Internal API types for experiment registration.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::experiments::metadata::RepoInfo;

/// Request body for experiment registration.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExperimentRegisterRequest {
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
