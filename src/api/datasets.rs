//! Dataset registration and summary endpoint contracts.
use super::{Client, Error, Plane};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Request body for dataset registration.
///
/// Create the request with its required names, then set optional fields:
///
/// ```
/// use braintrust_sdk_rust::api::datasets::DatasetRegisterRequest;
/// let mut request = DatasetRegisterRequest::new("project", "organization", "dataset");
/// request.set_description(Some("Evaluation cases".into()));
/// ```
///
/// Struct literals are intentionally unavailable outside this crate, allowing
/// additional fields to be introduced without breaking existing callers.
///
/// ```compile_fail
/// use braintrust_sdk_rust::api::datasets::DatasetRegisterRequest;
/// let _ = DatasetRegisterRequest {
///     project_name: "project".into(), org_name: "organization".into(),
///     dataset_name: "dataset".into(), description: None, metadata: None,
/// };
/// ```
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct DatasetRegisterRequest {
    pub(crate) project_name: String,
    pub(crate) org_name: String,
    pub(crate) dataset_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) metadata: Option<Map<String, Value>>,
}

/// Response from dataset registration.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct DatasetRegisterResponse {
    pub(crate) project: DatasetProjectInfo,
    pub(crate) dataset: DatasetInfo,
}

/// Project info from registration response.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct DatasetProjectInfo {
    pub(crate) id: String,
    pub(crate) name: String,
}

/// Dataset info from registration response.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct DatasetInfo {
    pub(crate) id: String,
    pub(crate) name: String,
}

/// Response from dataset summary API.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct DatasetSummaryResponse {
    pub(crate) project_name: String,
    pub(crate) dataset_name: String,
    pub(crate) project_id: String,
    pub(crate) dataset_id: String,
    pub(crate) project_url: Option<String>,
    pub(crate) dataset_url: Option<String>,
    pub(crate) data_summary: Option<DataSummaryStats>,
}

/// Data summary statistics.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct DataSummaryStats {
    pub(crate) total_records: Option<u64>,
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

impl DatasetRegisterRequest {
    /// Construct a registration request with required project, organization, and dataset names.
    pub fn new(
        project_name: impl Into<String>,
        org_name: impl Into<String>,
        dataset_name: impl Into<String>,
    ) -> Self {
        Self {
            project_name: project_name.into(),
            org_name: org_name.into(),
            dataset_name: dataset_name.into(),
            description: None,
            metadata: None,
        }
    }
}

impl DatasetRegisterRequest {
    /// Return the project name.
    pub fn project_name(&self) -> &str {
        &self.project_name
    }
    /// Set the project name.
    pub fn set_project_name(&mut self, value: impl Into<String>) -> &mut Self {
        self.project_name = value.into();
        self
    }
    /// Return the org name.
    pub fn org_name(&self) -> &str {
        &self.org_name
    }
    /// Set the org name.
    pub fn set_org_name(&mut self, value: impl Into<String>) -> &mut Self {
        self.org_name = value.into();
        self
    }
    /// Return the dataset name.
    pub fn dataset_name(&self) -> &str {
        &self.dataset_name
    }
    /// Set the dataset name.
    pub fn set_dataset_name(&mut self, value: impl Into<String>) -> &mut Self {
        self.dataset_name = value.into();
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
    /// Return the metadata.
    pub fn metadata(&self) -> Option<&Map<String, Value>> {
        self.metadata.as_ref()
    }
    /// Set the metadata.
    pub fn set_metadata(&mut self, value: Option<Map<String, Value>>) -> &mut Self {
        self.metadata = value;
        self
    }
}

impl DatasetRegisterResponse {
    /// Return the project.
    pub fn project(&self) -> &DatasetProjectInfo {
        &self.project
    }
    /// Return the dataset.
    pub fn dataset(&self) -> &DatasetInfo {
        &self.dataset
    }
}

impl DatasetProjectInfo {
    /// Return the id.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Return the name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl DatasetInfo {
    /// Return the id.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Return the name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl DatasetSummaryResponse {
    /// Return the project name.
    pub fn project_name(&self) -> &str {
        &self.project_name
    }
    /// Return the dataset name.
    pub fn dataset_name(&self) -> &str {
        &self.dataset_name
    }
    /// Return the project id.
    pub fn project_id(&self) -> &str {
        &self.project_id
    }
    /// Return the dataset id.
    pub fn dataset_id(&self) -> &str {
        &self.dataset_id
    }
    /// Return the project url.
    pub fn project_url(&self) -> Option<&str> {
        self.project_url.as_deref()
    }
    /// Return the dataset url.
    pub fn dataset_url(&self) -> Option<&str> {
        self.dataset_url.as_deref()
    }
    /// Return the data summary.
    pub fn data_summary(&self) -> Option<&DataSummaryStats> {
        self.data_summary.as_ref()
    }
}

impl DataSummaryStats {
    /// Return the total records.
    pub fn total_records(&self) -> Option<&u64> {
        self.total_records.as_ref()
    }
}
