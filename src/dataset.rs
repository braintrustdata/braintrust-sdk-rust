//! Dataset support for managing test/evaluation data.
//!
//! Datasets allow you to:
//! - Create and manage versioned collections of test records
//! - Insert, update, and delete records
//! - Fetch records with pagination
//!
//! # Example
//!
//! ```ignore
//! let dataset = client
//!     .dataset_builder()
//!     .await?
//!     .project_name("my-project")
//!     .dataset_name("test-cases")
//!     .build()?;
//!
//! let id = dataset.insert(
//!     DatasetInsert::builder()
//!         .input(json!({"question": "What is 2+2?"}))
//!         .expected(json!({"answer": "4"}))
//!         .build()?
//! ).await?;
//! ```

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use futures::{future::BoxFuture, FutureExt};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::error::{BraintrustError, Result};
use crate::http::api_result;
use crate::internal::execution::ExecutionBinding;
use crate::internal::session::CredentialScope;
use crate::Completion;

// ============================================================================
// Dataset Insert Types
// ============================================================================

/// Fields stored in a dataset record. Absent fields are omitted from the wire.
///
/// ```
/// use braintrust_sdk_rust::DatasetFields;
/// use serde_json::json;
/// let fields = DatasetFields::builder().input(json!(null)).tags(vec!["test".into()]).build();
/// assert_eq!(fields.input(), Some(&json!(null)));
/// assert!(fields.expected().is_none());
/// ```
///
/// Construct fields through the builder rather than depending on their layout:
///
/// ```compile_fail
/// use braintrust_sdk_rust::DatasetFields;
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

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DatasetWrite {
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
}

// ============================================================================
// Dataset Insert Types
// ============================================================================

/// Data to insert into a dataset. Input is required, including when its value is JSON null.
///
/// ```
/// use braintrust_sdk_rust::DatasetInsert;
/// use serde_json::json;
/// let record = DatasetInsert::builder().input(json!(null)).build();
/// assert_eq!(record.input(), &json!(null));
/// ```
///
/// Missing input is rejected at compile time:
///
/// ```compile_fail
/// use braintrust_sdk_rust::DatasetInsert;
/// let record = DatasetInsert::builder().expected(serde_json::json!(4)).build();
/// ```
///
/// There is no default record that bypasses the required input:
///
/// ```compile_fail
/// use braintrust_sdk_rust::DatasetInsert;
/// let record = DatasetInsert::default();
/// ```
#[derive(bon::Builder, Debug, Clone)]
pub struct DatasetInsert {
    #[builder(into)]
    input: Value,
    #[builder(into)]
    expected: Option<Value>,
    metadata: Option<Map<String, Value>>,
    tags: Option<Vec<String>>,
}

impl DatasetInsert {
    /// Return the input, including an explicit JSON null.
    pub fn input(&self) -> &Value {
        &self.input
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

impl From<DatasetInsert> for DatasetFields {
    fn from(record: DatasetInsert) -> Self {
        Self {
            input: Some(record.input),
            expected: record.expected,
            metadata: record.metadata,
            tags: record.tags,
        }
    }
}

// ============================================================================
// Dataset Record
// ============================================================================

/// A record within a dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DatasetRecord {
    /// Unique record ID.
    pub(crate) id: String,
    /// Input data for the operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input: Option<Value>,
    /// Expected output for evaluation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expected: Option<Value>,
    /// Key-value metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) metadata: Option<Map<String, Value>>,
    /// String tags for filtering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tags: Option<Vec<String>>,
    /// Transaction ID (for versioning).
    #[serde(rename = "_xact_id", skip_serializing_if = "Option::is_none")]
    pub(crate) xact_id: Option<String>,
    /// ISO 8601 timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) created: Option<String>,
}

/// BTQL query for fetching dataset records.
#[derive(bon::Builder, Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct BTQLQuery {
    pub(crate) query: BTQLQueryInner,
}

/// Inner BTQL query structure.
#[derive(bon::Builder, Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct BTQLQueryInner {
    /// Dataset reference: dataset('id')
    #[builder(into)]
    pub(crate) from: String,
    /// Maximum records per page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) limit: Option<usize>,
    /// Pagination cursor token.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(into)]
    pub(crate) cursor: Option<String>,
}

/// Response from BTQL query.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct BTQLResponse {
    /// Records in JSONL format.
    #[serde(default)]
    pub(crate) data: Vec<DatasetRecord>,
    /// Cursor for next page (None if last page).
    pub(crate) cursor: Option<String>,
}

// ============================================================================
// Dataset Summary
// ============================================================================

/// Summary statistics about a dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DatasetSummary {
    project_name: String,
    dataset_name: String,
    project_id: Option<String>,
    dataset_id: Option<String>,
    project_url: Option<String>,
    dataset_url: Option<String>,
    total_records: Option<u64>,
}

impl DatasetSummary {
    /// Create a new DatasetSummary (internal use).
    #[allow(dead_code)]
    pub(crate) fn new(project_name: String, dataset_name: String) -> Self {
        Self {
            project_name,
            dataset_name,
            project_id: None,
            dataset_id: None,
            project_url: None,
            dataset_url: None,
            total_records: None,
        }
    }

    /// Get the project name.
    pub fn project_name(&self) -> &str {
        &self.project_name
    }

    /// Get the dataset name.
    pub fn dataset_name(&self) -> &str {
        &self.dataset_name
    }

    /// Get the project ID (if known).
    pub fn project_id(&self) -> Option<&str> {
        self.project_id.as_deref()
    }

    /// Get the dataset ID (if known).
    pub fn dataset_id(&self) -> Option<&str> {
        self.dataset_id.as_deref()
    }

    /// Get the project URL (if available).
    pub fn project_url(&self) -> Option<&str> {
        self.project_url.as_deref()
    }

    /// Get the dataset URL (if available).
    pub fn dataset_url(&self) -> Option<&str> {
        self.dataset_url.as_deref()
    }

    /// Get the total number of records.
    pub fn total_records(&self) -> Option<u64> {
        self.total_records
    }
}

// ============================================================================
// Dataset Registration Types (Internal)
// ============================================================================

/// Metadata about a dataset, cached after registration.
#[derive(Debug, Clone)]
pub(crate) struct DatasetMetadata {
    pub dataset_id: String,
    #[allow(dead_code)]
    pub project_id: String,
}

use crate::api::DatasetRegisterRequest;

/// A dataset for storing test/evaluation data.
///
/// Datasets are lazily registered with the API on first use.
/// The builder requires both a project name and a dataset name:
///
/// ```compile_fail
/// # fn missing_project(client: &braintrust_sdk_rust::BraintrustClient) {
/// client.dataset_builder_with_credentials("token", "tenant").dataset_name("d").build();
/// # }
/// ```
///
/// ```compile_fail
/// # fn missing_dataset(client: &braintrust_sdk_rust::BraintrustClient) {
/// client.dataset_builder_with_credentials("token", "tenant").project_name("p").build();
/// # }
/// ```
#[derive(bon::Builder, Clone)]
#[builder(start_fn(name = new, vis = "pub(crate)"), derive(Clone))]
pub struct Dataset {
    #[builder(start_fn)]
    api: reqwest::Client,
    #[builder(start_fn)]
    execution: ExecutionBinding,
    #[builder(start_fn)]
    scope: CredentialScope,
    /// Project containing the dataset (required).
    #[builder(into)]
    project_name: String,
    /// Dataset name (required).
    #[builder(into)]
    dataset_name: String,
    #[builder(into)]
    description: Option<String>,
    metadata: Option<Map<String, Value>>,
    #[builder(skip)]
    lazy_metadata: Arc<OnceCell<DatasetMetadata>>,
}

impl fmt::Debug for Dataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dataset")
            .field("project_name", &self.project_name)
            .field("dataset_name", &self.dataset_name)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

impl Dataset {
    /// Ensure the dataset is registered, returning cached metadata.
    async fn ensure_registered(&self) -> Result<&DatasetMetadata> {
        self.lazy_metadata
            .get_or_try_init(|| async {
                let mut request = DatasetRegisterRequest::new(
                    self.project_name.clone(),
                    self.scope.org_name().unwrap_or_default().to_owned(),
                    self.dataset_name.clone(),
                );
                request.description = self.description.clone();
                request.metadata = self.metadata.clone();
                let response =
                    api_result(self.scope.api(&self.api)?.register_dataset(&request).await)?
                        .into_inner();
                Ok(DatasetMetadata {
                    dataset_id: response.dataset.id,
                    project_id: response.project.id,
                })
            })
            .await
    }

    /// Get the dataset ID (triggers registration if needed).
    pub fn dataset_id(&self) -> Completion<String, BraintrustError> {
        let dataset = self.clone();
        self.execution
            .response(async move { dataset.dataset_id_inner().await })
    }

    async fn dataset_id_inner(&self) -> Result<String> {
        Ok(self.ensure_registered().await?.dataset_id.clone())
    }

    /// Get the dataset name.
    pub fn dataset_name(&self) -> &str {
        &self.dataset_name
    }

    /// Get the project name.
    pub fn project_name(&self) -> &str {
        &self.project_name
    }

    /// Insert a record and return its generated ID after the API acknowledges it.
    pub fn insert(&self, record: DatasetInsert) -> Completion<String, BraintrustError> {
        let dataset = self.clone();
        self.execution
            .response(async move { dataset.insert_inner(record).await })
    }

    async fn insert_inner(&self, record: DatasetInsert) -> Result<String> {
        let dataset_id = self.ensure_registered().await?.dataset_id.clone();
        let id = Uuid::new_v4().to_string();
        self.write_record(DatasetWrite::insert(dataset_id, id.clone(), record.into()))
            .await?;
        Ok(id)
    }

    /// Merge supplied fields into a record, returning after API acknowledgement.
    pub fn update(&self, id: &str, fields: DatasetFields) -> Completion<(), BraintrustError> {
        let dataset = self.clone();
        let id = id.to_owned();
        self.execution
            .response(async move { dataset.update_inner(&id, fields).await })
    }

    async fn update_inner(&self, id: &str, fields: DatasetFields) -> Result<()> {
        let dataset_id = self.ensure_registered().await?.dataset_id.clone();
        self.write_record(DatasetWrite::update(dataset_id, id, fields))
            .await
    }

    /// Delete a record, returning after API acknowledgement.
    pub fn delete(&self, id: &str) -> Completion<(), BraintrustError> {
        let dataset = self.clone();
        let id = id.to_owned();
        self.execution
            .response(async move { dataset.delete_inner(&id).await })
    }

    async fn delete_inner(&self, id: &str) -> Result<()> {
        let dataset_id = self.ensure_registered().await?.dataset_id.clone();
        self.write_record(DatasetWrite::delete(dataset_id, id))
            .await
    }

    /// Fetch records with pagination.
    ///
    /// Returns an iterator that fetches records in batches.
    pub fn fetch(&self, batch_size: Option<usize>) -> Completion<DatasetIterator, BraintrustError> {
        let dataset = self.clone();
        self.execution
            .response(async move { dataset.fetch_inner(batch_size).await })
    }

    async fn fetch_inner(&self, batch_size: Option<usize>) -> Result<DatasetIterator> {
        let metadata = self.ensure_registered().await?;
        Ok(DatasetIterator::new(
            self.api.clone(),
            self.execution.clone(),
            self.scope.clone(),
            metadata.dataset_id.clone(),
            batch_size.unwrap_or(1000).max(1),
        ))
    }

    /// Get dataset summary statistics.
    pub fn summarize(&self) -> Completion<DatasetSummary, BraintrustError> {
        let dataset = self.clone();
        self.execution
            .response(async move { dataset.summarize_inner().await })
    }

    async fn summarize_inner(&self) -> Result<DatasetSummary> {
        let metadata = self.ensure_registered().await?;

        // Fetch summary from API
        let response = api_result(
            self.scope
                .api(&self.api)?
                .dataset_summary(&metadata.dataset_id)
                .await,
        )?
        .into_inner();

        Ok(DatasetSummary {
            project_name: response.project_name,
            dataset_name: response.dataset_name,
            project_id: Some(response.project_id),
            dataset_id: Some(response.dataset_id),
            project_url: response.project_url,
            dataset_url: response.dataset_url,
            total_records: response.data_summary.and_then(|ds| ds.total_records),
        })
    }

    async fn write_record(&self, row: DatasetWrite) -> Result<()> {
        api_result(
            self.scope
                .api(&self.api)?
                .logs3(&serde_json::json!({"api_version": crate::types::LOGS_API_VERSION, "rows": [row]}))
                .await,
        )?;
        Ok(())
    }
}

// ============================================================================
// DatasetIterator
// ============================================================================

struct DatasetCursor {
    fetcher: reqwest::Client,
    scope: CredentialScope,
    dataset_id: String,
    batch_size: usize,
    cursor: Option<String>,
    buffer: VecDeque<DatasetRecord>,
    done: bool,
}

/// Iterator for fetching dataset records with pagination.
///
/// ```no_run
/// use braintrust_sdk_rust::{BraintrustError, Dataset};
/// # async fn read(dataset: &Dataset) -> Result<(), BraintrustError> {
/// let mut records = dataset.fetch(Some(100)).await?;
/// while let Some(record) = records.next().await? {
///     println!("{}", record.id());
/// }
/// # Ok(())
/// # }
/// ```
pub struct DatasetIterator {
    execution: ExecutionBinding,
    cursor: Arc<tokio::sync::Mutex<DatasetCursor>>,
    tail: futures::future::Shared<BoxFuture<'static, ()>>,
}

impl DatasetIterator {
    fn new(
        fetcher: reqwest::Client,
        execution: ExecutionBinding,
        scope: CredentialScope,
        dataset_id: String,
        batch_size: usize,
    ) -> Self {
        Self {
            execution,
            tail: futures::future::ready(()).boxed().shared(),
            cursor: Arc::new(tokio::sync::Mutex::new(DatasetCursor {
                fetcher,
                scope,
                dataset_id,
                batch_size,
                cursor: None,
                buffer: VecDeque::new(),
                done: false,
            })),
        }
    }

    /// Submit a request for the next record, fetching a page if needed.
    /// Accepted cursor operations run in issuance order; dropping a token still consumes
    /// its record. An API error preserves the cursor for a later retry.
    #[allow(clippy::should_implement_trait)] // Observation is async, unlike Iterator::next.
    pub fn next(&mut self) -> Completion<Option<DatasetRecord>, BraintrustError> {
        self.submit(|mut cursor| async move { cursor.next().await })
    }

    /// Submit collection of all remaining records. Dropping the returned token
    /// does not cancel collection; page failures remain operation errors.
    pub fn collect(mut self) -> Completion<Vec<DatasetRecord>, BraintrustError> {
        self.submit(|mut cursor| async move {
            let mut records = Vec::new();
            while let Some(record) = cursor.next().await? {
                records.push(record);
            }
            Ok(records)
        })
    }

    fn submit<T, F, Fut>(&mut self, work: F) -> Completion<T, BraintrustError>
    where
        T: Send + 'static,
        F: FnOnce(tokio::sync::OwnedMutexGuard<DatasetCursor>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        let cursor = self.cursor.clone();
        let previous = self.tail.clone();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let completion = self.execution.response(async move {
            // Holding the sender across the whole operation preserves issuance
            // order even if runtime task polling is reordered. Drop releases
            // the next operation on success, error, panic or cancellation.
            let _release = release;
            previous.await;
            work(cursor.lock_owned().await).await
        });
        if completion.is_ok() {
            self.tail = async move {
                let _ = released.await;
            }
            .boxed()
            .shared();
        }
        // Rejected work leaves the existing chain and cursor untouched.
        completion
    }
}

impl DatasetCursor {
    /// Get the next record, fetching a new batch if needed.
    async fn next(&mut self) -> Result<Option<DatasetRecord>> {
        loop {
            if let Some(record) = self.buffer.pop_front() {
                return Ok(Some(record));
            }
            if self.done {
                return Ok(None);
            }
            let previous_cursor = self.cursor.clone();
            let query = BTQLQuery {
                query: BTQLQueryInner {
                    from: format!("dataset('{}')", self.dataset_id),
                    limit: Some(self.batch_size),
                    cursor: previous_cursor.clone(),
                },
            };
            // A failed request leaves the cursor and EOF state untouched.
            let response = api_result(
                self.scope
                    .api(&self.fetcher)?
                    .btql_json_as::<_, BTQLResponse>(&query)
                    .await,
            )?
            .into_inner();
            if response.cursor.is_some() && response.cursor == previous_cursor {
                return Err(crate::BraintrustError::Background(
                    "dataset pagination cursor did not advance".into(),
                ));
            }
            self.cursor = response.cursor;
            self.done = self.cursor.is_none();
            self.buffer.extend(response.data);
            // Empty pages with a continuation are not end-of-data.
        }
    }
}

impl DatasetRecord {
    /// Return the id.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Return the input.
    pub fn input(&self) -> Option<&Value> {
        self.input.as_ref()
    }
    /// Return the expected.
    pub fn expected(&self) -> Option<&Value> {
        self.expected.as_ref()
    }
    /// Return the metadata.
    pub fn metadata(&self) -> Option<&Map<String, Value>> {
        self.metadata.as_ref()
    }
    /// Return the tags.
    pub fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
    /// Return the xact id.
    pub fn xact_id(&self) -> Option<&str> {
        self.xact_id.as_deref()
    }
    /// Return the created.
    pub fn created(&self) -> Option<&str> {
        self.created.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dataset_summary_getters() {
        let mut summary = DatasetSummary::new("project".to_string(), "dataset".to_string());
        summary.project_id = Some("proj-id".to_string());
        summary.dataset_id = Some("ds-id".to_string());
        summary.total_records = Some(100);

        assert_eq!(summary.project_name(), "project");
        assert_eq!(summary.dataset_name(), "dataset");
        assert_eq!(summary.project_id(), Some("proj-id"));
        assert_eq!(summary.dataset_id(), Some("ds-id"));
        assert_eq!(summary.total_records(), Some(100));
    }
}
