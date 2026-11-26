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
//! ).await;
//!
//! dataset.flush().await?;
//! ```

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{Mutex, OnceCell};
use uuid::Uuid;

use crate::error::Result;
use crate::log_queue::Credential;
use crate::span::SpanSubmitter;
use crate::types::{ParentSpanInfo, SpanPayload};

// ============================================================================
// Dataset Insert Types
// ============================================================================

/// Error type for DatasetInsert builder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DatasetInsertBuilderError {
    /// Input is required but was not provided.
    MissingInput,
}

impl fmt::Display for DatasetInsertBuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingInput => write!(f, "input is required but was not provided"),
        }
    }
}

impl std::error::Error for DatasetInsertBuilderError {}

/// Data to insert into a dataset.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct DatasetInsert {
    pub(crate) input: Option<Value>,
    pub(crate) expected: Option<Value>,
    pub(crate) metadata: Option<Map<String, Value>>,
    pub(crate) tags: Option<Vec<String>>,
}

impl DatasetInsert {
    /// Create a new DatasetInsert builder.
    pub fn builder() -> DatasetInsertBuilder {
        DatasetInsertBuilder::new()
    }
}

/// Builder for DatasetInsert.
#[derive(Clone, Default)]
pub struct DatasetInsertBuilder {
    inner: DatasetInsert,
}

impl DatasetInsertBuilder {
    /// Create a new DatasetInsertBuilder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the input data (required).
    pub fn input(mut self, input: impl Into<Value>) -> Self {
        self.inner.input = Some(input.into());
        self
    }

    /// Set the expected output for evaluation.
    pub fn expected(mut self, expected: impl Into<Value>) -> Self {
        self.inner.expected = Some(expected.into());
        self
    }

    /// Set the metadata map.
    pub fn metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.inner.metadata = Some(metadata);
        self
    }

    /// Add a single metadata entry.
    pub fn metadata_entry(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.inner
            .metadata
            .get_or_insert_with(Map::new)
            .insert(key.into(), value.into());
        self
    }

    /// Set multiple tags at once.
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.inner.tags = Some(tags);
        self
    }

    /// Add a single tag.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.inner
            .tags
            .get_or_insert_with(Vec::new)
            .push(tag.into());
        self
    }

    /// Build the DatasetInsert.
    pub fn build(self) -> std::result::Result<DatasetInsert, DatasetInsertBuilderError> {
        if self.inner.input.is_none() {
            return Err(DatasetInsertBuilderError::MissingInput);
        }
        Ok(self.inner)
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
    pub id: String,
    /// Input data for the operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// Expected output for evaluation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
    /// Key-value metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    /// String tags for filtering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Transaction ID (for versioning).
    #[serde(rename = "_xact_id", skip_serializing_if = "Option::is_none")]
    pub xact_id: Option<String>,
    /// ISO 8601 timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
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

pub(crate) use crate::api::{DatasetRegisterRequest, DatasetRegisterResponse};

// ============================================================================
// BTQL Query Types (Internal)
// ============================================================================

/// BTQL query for fetching dataset records.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct BTQLQuery {
    pub query: BTQLQueryInner,
}

/// Inner BTQL query structure.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct BTQLQueryInner {
    /// Dataset reference: dataset('id')
    pub from: String,
    /// Maximum records per page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Pagination cursor token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Response from BTQL query.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BTQLResponse {
    /// Records in JSONL format.
    #[serde(default)]
    pub data: Vec<DatasetRecord>,
    /// Cursor for next page (None if last page).
    pub cursor: Option<String>,
}

// ============================================================================
// Dataset Summary API Types (Internal)
// ============================================================================

pub(crate) use crate::api::DatasetSummaryResponse;

// ============================================================================
// Traits for API Operations
// ============================================================================

/// Trait for registering datasets.
#[async_trait]
pub(crate) trait DatasetRegistrar: Send + Sync {
    /// Register a dataset with the API.
    async fn register_dataset(
        &self,
        token: &str,
        request: DatasetRegisterRequest,
    ) -> Result<DatasetRegisterResponse>;
}

/// Trait for fetching dataset records via BTQL.
#[async_trait]
pub(crate) trait DatasetFetcher: Send + Sync {
    /// Fetch dataset records via BTQL query.
    async fn fetch_dataset_records(&self, token: &str, query: BTQLQuery) -> Result<BTQLResponse>;
}

/// Trait for fetching dataset summary.
#[async_trait]
pub(crate) trait DatasetSummarizer: Send + Sync {
    /// Fetch dataset summary.
    async fn fetch_dataset_summary(
        &self,
        token: &str,
        dataset_id: &str,
    ) -> Result<DatasetSummaryResponse>;
}

// ============================================================================
// DatasetBuilder Error Types
// ============================================================================

/// Error type for DatasetBuilder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DatasetBuilderError {
    /// Project name is required but was not provided.
    MissingProjectName,
    /// Dataset name is required but was not provided.
    MissingDatasetName,
}

impl fmt::Display for DatasetBuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProjectName => {
                write!(f, "project_name is required but was not provided")
            }
            Self::MissingDatasetName => {
                write!(f, "dataset_name is required but was not provided")
            }
        }
    }
}

impl std::error::Error for DatasetBuilderError {}

// ============================================================================
// DatasetBuilder
// ============================================================================

/// Builder for creating datasets with lazy registration.
#[allow(private_bounds)]
pub struct DatasetBuilder<S: SpanSubmitter + DatasetRegistrar + DatasetFetcher + DatasetSummarizer>
{
    submitter: Arc<S>,
    token: String,
    org_name: String,
    org_id: String,
    project_name: Option<String>,
    dataset_name: Option<String>,
    description: Option<String>,
    metadata: Option<Map<String, Value>>,
}

impl<S: SpanSubmitter + DatasetRegistrar + DatasetFetcher + DatasetSummarizer> Clone
    for DatasetBuilder<S>
{
    fn clone(&self) -> Self {
        Self {
            submitter: Arc::clone(&self.submitter),
            token: self.token.clone(),
            org_name: self.org_name.clone(),
            org_id: self.org_id.clone(),
            project_name: self.project_name.clone(),
            dataset_name: self.dataset_name.clone(),
            description: self.description.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

#[allow(private_bounds)]
impl<S: SpanSubmitter + DatasetRegistrar + DatasetFetcher + DatasetSummarizer + 'static>
    DatasetBuilder<S>
{
    /// Create a new DatasetBuilder (internal use).
    pub(crate) fn new(
        submitter: Arc<S>,
        token: impl Into<String>,
        org_name: impl Into<String>,
    ) -> Self {
        Self {
            submitter,
            token: token.into(),
            org_name: org_name.into(),
            org_id: String::new(),
            project_name: None,
            dataset_name: None,
            description: None,
            metadata: None,
        }
    }

    pub(crate) fn org_id(mut self, org_id: String) -> Self {
        self.org_id = org_id;
        self
    }

    /// Set the project name (required).
    pub fn project_name(mut self, project_name: impl Into<String>) -> Self {
        self.project_name = Some(project_name.into());
        self
    }

    /// Set the dataset name (required).
    pub fn dataset_name(mut self, dataset_name: impl Into<String>) -> Self {
        self.dataset_name = Some(dataset_name.into());
        self
    }

    /// Set the dataset description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the dataset metadata.
    pub fn metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Add a single metadata entry.
    pub fn metadata_entry(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.metadata
            .get_or_insert_with(Map::new)
            .insert(key.into(), value.into());
        self
    }

    /// Build the dataset (lazy registration).
    pub fn build(self) -> std::result::Result<Dataset<S>, DatasetBuilderError> {
        let project_name = self
            .project_name
            .ok_or(DatasetBuilderError::MissingProjectName)?;
        let dataset_name = self
            .dataset_name
            .ok_or(DatasetBuilderError::MissingDatasetName)?;

        Ok(Dataset {
            submitter: self.submitter,
            token: self.token,
            org_name: self.org_name,
            org_id: self.org_id,
            project_name,
            dataset_name,
            description: self.description,
            metadata: self.metadata,
            lazy_metadata: Arc::new(OnceCell::new()),
            pending_rows: Default::default(),
        })
    }
}

// ============================================================================
// Dataset
// ============================================================================

/// A dataset for storing test/evaluation data.
///
/// Datasets are lazily registered with the API on first use.
#[allow(private_bounds)]
pub struct Dataset<S: SpanSubmitter + DatasetRegistrar + DatasetFetcher + DatasetSummarizer> {
    submitter: Arc<S>,
    token: String,
    org_name: String,
    org_id: String,
    project_name: String,
    dataset_name: String,
    description: Option<String>,
    metadata: Option<Map<String, Value>>,
    lazy_metadata: Arc<OnceCell<DatasetMetadata>>,
    pending_rows: Arc<std::sync::Mutex<Vec<PendingDatasetRow>>>,
}

impl<S: SpanSubmitter + DatasetRegistrar + DatasetFetcher + DatasetSummarizer> Clone
    for Dataset<S>
{
    fn clone(&self) -> Self {
        Self {
            submitter: self.submitter.clone(),
            token: self.token.clone(),
            org_name: self.org_name.clone(),
            org_id: self.org_id.clone(),
            project_name: self.project_name.clone(),
            dataset_name: self.dataset_name.clone(),
            description: self.description.clone(),
            metadata: self.metadata.clone(),
            lazy_metadata: self.lazy_metadata.clone(),
            pending_rows: self.pending_rows.clone(),
        }
    }
}

impl<S: SpanSubmitter + DatasetRegistrar + DatasetFetcher + DatasetSummarizer> fmt::Debug
    for Dataset<S>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dataset")
            .field("project_name", &self.project_name)
            .field("dataset_name", &self.dataset_name)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

/// A pending dataset row waiting to be submitted.
struct PendingDatasetRow {
    id: String,
    input: Option<Value>,
    expected: Option<Value>,
    metadata: Option<Map<String, Value>>,
    tags: Option<Vec<String>>,
    is_merge: bool,
    is_delete: bool,
}

#[allow(private_bounds)]
impl<S: SpanSubmitter + DatasetRegistrar + DatasetFetcher + DatasetSummarizer + 'static>
    Dataset<S>
{
    /// Ensure the dataset is registered, returning cached metadata.
    async fn ensure_registered(&self) -> Result<&DatasetMetadata> {
        self.lazy_metadata
            .get_or_try_init(|| async {
                let mut request = DatasetRegisterRequest::new(
                    self.project_name.clone(),
                    self.org_name.clone(),
                    self.dataset_name.clone(),
                );
                request.description = self.description.clone();
                request.metadata = self.metadata.clone();

                match self.submitter.register_dataset(&self.token, request).await {
                    Ok(response) => Ok(DatasetMetadata {
                        dataset_id: response.dataset.id,
                        project_id: response.project.id,
                    }),
                    Err(e) => Err(e),
                }
            })
            .await
    }

    /// Get the dataset ID (triggers registration if needed).
    pub fn dataset_id(&self) -> crate::Completion<String, crate::BraintrustError> {
        let dataset = self.clone();
        crate::service::submit(
            async move { Ok(dataset.ensure_registered().await?.dataset_id.clone()) },
        )
    }

    /// Get the dataset name.
    pub fn dataset_name(&self) -> &str {
        &self.dataset_name
    }

    /// Get the project name.
    pub fn project_name(&self) -> &str {
        &self.project_name
    }

    /// Insert a new record into the dataset.
    ///
    /// Returns the auto-generated record ID.
    pub fn insert(
        &self,
        record: DatasetInsert,
    ) -> crate::Completion<String, crate::BraintrustError> {
        // Queued now, so a flush started after this call includes the row.
        let id = Uuid::new_v4().to_string();
        self.pending_rows.lock().unwrap().push(PendingDatasetRow {
            id: id.clone(),
            input: record.input,
            expected: record.expected,
            metadata: record.metadata,
            tags: record.tags,
            is_merge: false,
            is_delete: false,
        });
        crate::Completion::finished(Ok(id))
    }

    /// Update an existing record by ID (merge semantics).
    ///
    /// Unspecified fields are preserved.
    pub fn update(
        &self,
        id: &str,
        record: DatasetInsert,
    ) -> crate::Completion<(), crate::BraintrustError> {
        self.pending_rows.lock().unwrap().push(PendingDatasetRow {
            id: id.to_owned(),
            input: record.input,
            expected: record.expected,
            metadata: record.metadata,
            tags: record.tags,
            is_merge: true,
            is_delete: false,
        });
        crate::Completion::finished(Ok(()))
    }

    /// Delete a record by ID.
    pub fn delete(&self, id: &str) -> crate::Completion<(), crate::BraintrustError> {
        self.pending_rows.lock().unwrap().push(PendingDatasetRow {
            id: id.to_owned(),
            input: None,
            expected: None,
            metadata: None,
            tags: None,
            is_merge: false,
            is_delete: true,
        });
        crate::Completion::finished(Ok(()))
    }

    /// Fetch records with pagination.
    ///
    /// Returns an iterator that fetches records in batches.
    pub fn fetch(
        &self,
        batch_size: Option<usize>,
    ) -> crate::Completion<DatasetIterator<S>, crate::BraintrustError> {
        let dataset = self.clone();
        crate::service::submit(async move {
            let metadata = dataset.ensure_registered().await?;
            Ok(DatasetIterator::new(
                Arc::clone(&dataset.submitter),
                dataset.token.clone(),
                metadata.dataset_id.clone(),
                batch_size.unwrap_or(1000),
            ))
        })
    }

    /// Get dataset summary statistics.
    pub fn summarize(&self) -> crate::Completion<DatasetSummary, crate::BraintrustError> {
        let dataset = self.clone();
        crate::service::submit(async move {
            // Flush pending operations first
            dataset.flush().await?;

            let metadata = dataset.ensure_registered().await?;

            // Fetch summary from API
            let response = dataset
                .submitter
                .fetch_dataset_summary(&dataset.token, &metadata.dataset_id)
                .await?;

            Ok(DatasetSummary {
                project_name: response.project_name,
                dataset_name: response.dataset_name,
                project_id: Some(response.project_id),
                dataset_id: Some(response.dataset_id),
                project_url: response.project_url,
                dataset_url: response.dataset_url,
                total_records: response.data_summary.and_then(|ds| ds.total_records),
            })
        })
    }

    /// Flush this dataset's writes made before this call.
    pub fn flush(&self) -> crate::Completion<(), crate::BraintrustError> {
        let dataset = self.clone();
        let rows = std::mem::take(&mut *self.pending_rows.lock().unwrap());
        crate::service::submit(async move {
            let metadata = dataset.ensure_registered().await?;

            if rows.is_empty() {
                return dataset
                    .submitter
                    .flush()
                    .await
                    .map(|_| ())
                    .map_err(Into::into);
            }

            // Submit each row
            for row in rows {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();

                let payload = SpanPayload {
                    row_id: row.id.clone(),
                    span_id: row.id,
                    is_merge: row.is_merge,
                    span_components: None,
                    org_id: dataset.org_id.clone(),
                    org_name: Some(dataset.org_name.clone()),
                    project_name: Some(dataset.project_name.clone()),
                    input: row.input,
                    output: None,
                    expected: row.expected,
                    error: None,
                    scores: None,
                    metadata: row.metadata,
                    metrics: None,
                    tags: row.tags,
                    context: Some(serde_json::json!({
                        "calendar_time": now,
                    })),
                    span_attributes: if row.is_delete {
                        Some(crate::types::SpanAttributes {
                            name: None,
                            span_type: None,
                            purpose: None,
                            extra: {
                                let mut map = std::collections::HashMap::new();
                                map.insert("_object_delete".to_string(), Value::Bool(true));
                                map
                            },
                        })
                    } else {
                        None
                    },
                    extra: std::collections::HashMap::new(),
                };

                let parent_info = ParentSpanInfo::Dataset {
                    object_id: metadata.dataset_id.clone(),
                };

                dataset.submitter.submit(
                    Credential::token_value(dataset.token.clone()),
                    payload,
                    Some(parent_info),
                );
            }

            dataset
                .submitter
                .flush()
                .await
                .map(|_| ())
                .map_err(Into::into)
        })
    }
}

// ============================================================================
// DatasetIterator
// ============================================================================

/// Iterator for fetching dataset records with pagination.
#[allow(private_bounds)]
pub struct DatasetIterator<S: DatasetFetcher> {
    state: Arc<Mutex<DatasetIteratorState<S>>>,
}
struct DatasetIteratorState<S: DatasetFetcher> {
    fetcher: Arc<S>,
    token: String,
    dataset_id: String,
    batch_size: usize,
    cursor: Option<String>,
    buffer: VecDeque<DatasetRecord>,
    done: bool,
}

#[allow(private_bounds)]
impl<S: DatasetFetcher + 'static> DatasetIteratorState<S> {
    fn new(fetcher: Arc<S>, token: String, dataset_id: String, batch_size: usize) -> Self {
        Self {
            fetcher,
            token,
            dataset_id,
            batch_size,
            cursor: None,
            buffer: VecDeque::new(),
            done: false,
        }
    }

    /// Get the next record, fetching a new batch if needed.
    async fn next(&mut self) -> Option<DatasetRecord> {
        // Return buffered record if available
        if let Some(record) = self.buffer.pop_front() {
            return Some(record);
        }

        // If we've exhausted all pages, return None
        if self.done {
            return None;
        }

        // Fetch next batch
        let query = BTQLQuery {
            query: BTQLQueryInner {
                from: format!("dataset('{}')", self.dataset_id),
                limit: Some(self.batch_size),
                cursor: self.cursor.take(),
            },
        };

        match self.fetcher.fetch_dataset_records(&self.token, query).await {
            Ok(response) => {
                // Update cursor for next page
                self.cursor = response.cursor;
                self.done = self.cursor.is_none();

                // Buffer the records
                self.buffer.extend(response.data);

                // Return first record
                self.buffer.pop_front()
            }
            Err(e) => {
                tracing::warn!("failed to fetch dataset records: {}", e);
                self.done = true;
                None
            }
        }
    }
}
#[allow(private_bounds)]
impl<S: DatasetFetcher + 'static> DatasetIterator<S> {
    fn new(fetcher: Arc<S>, token: String, dataset_id: String, batch_size: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(DatasetIteratorState::new(
                fetcher, token, dataset_id, batch_size,
            ))),
        }
    }
    /// Issue the next page or buffered-record operation immediately.
    // This asynchronous cursor cannot implement the synchronous Iterator contract.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> crate::Completion<Option<DatasetRecord>, crate::BraintrustError> {
        let state = self.state.clone();
        crate::service::submit(async move { Ok(state.lock().await.next().await) })
    }
    /// Collect remaining records on the shared executor.
    pub fn collect(self) -> crate::Completion<Vec<DatasetRecord>, crate::BraintrustError> {
        crate::service::submit(async move {
            let mut state = self.state.lock().await;
            let mut records = Vec::new();
            while let Some(row) = state.next().await {
                records.push(row);
            }
            Ok(records)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dataset_insert_builder_requires_input() {
        let result = DatasetInsert::builder()
            .expected(serde_json::json!({"answer": "4"}))
            .build();

        assert!(matches!(
            result,
            Err(DatasetInsertBuilderError::MissingInput)
        ));
    }

    #[test]
    fn test_dataset_insert_builder_works() {
        let result = DatasetInsert::builder()
            .input(serde_json::json!({"question": "What is 2+2?"}))
            .expected(serde_json::json!({"answer": "4"}))
            .tag("math")
            .tag("addition")
            .metadata_entry("difficulty", serde_json::json!("easy"))
            .build();

        assert!(result.is_ok());
        let insert = result.unwrap();
        assert!(insert.input.is_some());
        assert!(insert.expected.is_some());
        assert_eq!(insert.tags.as_ref().map(|t| t.len()), Some(2));
    }

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
