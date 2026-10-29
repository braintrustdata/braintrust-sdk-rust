//! Dataset registration, row mutation, and summary endpoint contracts.
use super::{Client, Error, Plane};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Request body for dataset registration.
///
/// Create the request with its required names and optional fields:
///
/// ```
/// use braintrust_sdk_rust::api::datasets::DatasetRegisterRequest;
/// let request = DatasetRegisterRequest::builder()
///     .project_name("project").org_name("organization").dataset_name("dataset")
///     .description("Evaluation cases").build();
/// ```
///
/// Struct literals are intentionally unavailable outside this crate, allowing
/// additional fields to be introduced without breaking existing callers.
/// The builder also requires the three identifying names:
///
/// ```compile_fail
/// use braintrust_sdk_rust::api::datasets::DatasetRegisterRequest;
/// let _ = DatasetRegisterRequest::builder().project_name("project").build();
/// ```
///
/// ```compile_fail
/// use braintrust_sdk_rust::api::datasets::DatasetRegisterRequest;
/// let _ = DatasetRegisterRequest {
///     project_name: "project".into(), org_name: "organization".into(),
///     dataset_name: "dataset".into(), description: None, metadata: None,
/// };
/// ```
#[derive(bon::Builder, Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct DatasetRegisterRequest {
    #[builder(into)]
    pub(crate) project_name: String,
    #[builder(into)]
    pub(crate) org_name: String,
    #[builder(into)]
    pub(crate) dataset_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
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

/// Fields stored in a dataset record. Absent fields are omitted from the wire.
///
/// ```
/// use braintrust_sdk_rust::api::datasets::DatasetFields;
/// use serde_json::json;
/// let fields = DatasetFields::builder().input(json!(null)).tags(vec!["test".into()]).build();
/// assert_eq!(fields.input(), Some(&json!(null)));
/// assert!(fields.expected().is_none());
/// ```
///
/// Construct fields through the builder rather than depending on their layout:
///
/// ```compile_fail
/// use braintrust_sdk_rust::api::datasets::DatasetFields;
/// let mut fields = DatasetFields::default();
/// fields.input = Some(serde_json::json!(1));
/// ```
#[derive(bon::Builder, Debug, Clone, Default, Serialize)]
pub struct DatasetFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) expected: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) metadata: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tags: Option<Vec<String>>,
}
impl DatasetFields {
    /// Return the supplied input, including an explicit JSON null.
    pub fn input(&self) -> Option<&Value> {
        self.input.as_ref()
    }
    /// Return the expected output, if supplied.
    pub fn expected(&self) -> Option<&Value> {
        self.expected.as_ref()
    }
    /// Return the metadata entries, if supplied.
    pub fn metadata(&self) -> Option<&Map<String, Value>> {
        self.metadata.as_ref()
    }
    /// Return the tags, if supplied.
    pub fn tags(&self) -> Option<&[String]> {
        self.tags.as_deref()
    }
}

/// A dataset row mutation, independent of tracing/span payloads.
///
/// Use [`Self::insert`], [`Self::update`], or [`Self::delete`] to select the
/// operation. Its private fields keep wire flags and payload layout extensible.
///
/// ```compile_fail
/// use braintrust_sdk_rust::api::datasets::DatasetWrite;
/// let mut write = DatasetWrite::delete("dataset", "row");
/// write.id = "other".into();
/// ```
#[derive(Debug, Clone, Serialize)]
pub struct DatasetWrite {
    dataset_id: String,
    id: String,
    #[serde(flatten)]
    fields: Option<DatasetFields>,
    #[serde(rename = "_is_merge", skip_serializing_if = "Option::is_none")]
    is_merge: Option<bool>,
    #[serde(rename = "_object_delete", skip_serializing_if = "Option::is_none")]
    is_delete: Option<bool>,
}
impl DatasetWrite {
    /// Insert a record with the supplied ID and fields.
    pub fn insert(
        dataset_id: impl Into<String>,
        id: impl Into<String>,
        fields: DatasetFields,
    ) -> Self {
        Self {
            dataset_id: dataset_id.into(),
            id: id.into(),
            fields: Some(fields),
            is_merge: None,
            is_delete: None,
        }
    }
    /// Merge only the supplied fields into an existing record.
    pub fn update(
        dataset_id: impl Into<String>,
        id: impl Into<String>,
        fields: DatasetFields,
    ) -> Self {
        Self {
            is_merge: Some(true),
            ..Self::insert(dataset_id, id, fields)
        }
    }
    /// Delete a record without sending record fields.
    pub fn delete(dataset_id: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            dataset_id: dataset_id.into(),
            id: id.into(),
            fields: None,
            is_merge: None,
            is_delete: Some(true),
        }
    }
    /// Return the dataset receiving this mutation.
    pub fn dataset_id(&self) -> &str {
        &self.dataset_id
    }
    /// Return the record ID.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Return the supplied record fields. Deletes have no fields.
    pub fn fields(&self) -> Option<&DatasetFields> {
        self.fields.as_ref()
    }
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
    /// Send exactly one typed dataset mutation request on the data plane.
    /// No retry, buffering, splitting, or overflow upload is performed.
    pub async fn write(&self, rows: &[DatasetWrite]) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Request<'a> {
            rows: &'a [DatasetWrite],
            api_version: u8,
        }
        self.client
            .post_empty(
                Plane::Data,
                "logs3",
                &Request {
                    rows,
                    api_version: crate::types::LOGS_API_VERSION,
                },
            )
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
    /// Return the org name.
    pub fn org_name(&self) -> &str {
        &self.org_name
    }
    /// Return the dataset name.
    pub fn dataset_name(&self) -> &str {
        &self.dataset_name
    }
    /// Return the description.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
    /// Return the metadata.
    pub fn metadata(&self) -> Option<&Map<String, Value>> {
        self.metadata.as_ref()
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
