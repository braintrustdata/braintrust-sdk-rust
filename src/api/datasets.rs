//! Dataset registration and summary endpoint contracts.
use super::{Client, Error, Plane};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Request body for dataset registration.
#[derive(Debug, Clone, Serialize)]
pub struct DatasetRegisterRequest {
    pub project_name: String,
    pub org_name: String,
    pub dataset_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

/// Response from dataset registration.
#[derive(Debug, Clone, Deserialize)]
pub struct DatasetRegisterResponse {
    pub project: DatasetProjectInfo,
    pub dataset: DatasetInfo,
}

/// Project info from registration response.
#[derive(Debug, Clone, Deserialize)]
pub struct DatasetProjectInfo {
    pub id: String,
    pub name: String,
}

/// Dataset info from registration response.
#[derive(Debug, Clone, Deserialize)]
pub struct DatasetInfo {
    pub id: String,
    pub name: String,
}

/// Response from dataset summary API.
#[derive(Debug, Clone, Deserialize)]
pub struct DatasetSummaryResponse {
    pub project_name: String,
    pub dataset_name: String,
    pub project_id: String,
    pub dataset_id: String,
    pub project_url: Option<String>,
    pub dataset_url: Option<String>,
    pub data_summary: Option<DataSummaryStats>,
}

/// Data summary statistics.
#[derive(Debug, Clone, Deserialize)]
pub struct DataSummaryStats {
    pub total_records: Option<u64>,
}

#[derive(Clone)]
pub struct Datasets {
    pub(crate) client: Client,
}
impl Datasets {
    /// Register once on the control plane; no resolution cache or retry.
    pub async fn register(
        &self,
        request: DatasetRegisterRequest,
    ) -> Result<DatasetRegisterResponse, Error> {
        self.client
            .post_json(Plane::Control, "api/dataset/register", &request)
            .await
    }
    pub async fn summarize(&self, dataset_id: &str) -> Result<DatasetSummaryResponse, Error> {
        self.client
            .get_json(
                Plane::Data,
                "dataset-summary",
                &[("dataset_id", dataset_id)],
            )
            .await
    }
}
