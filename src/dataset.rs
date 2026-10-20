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

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::api::datasets::{DatasetFields, DatasetWrite};
use crate::error::Result;
use crate::internal::ports::{DatasetApi, DeliveryControl};
use crate::internal::session::CredentialScope;

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

impl From<DatasetInsert> for DatasetFields {
    fn from(record: DatasetInsert) -> Self {
        Self {
            input: record.input,
            expected: record.expected,
            metadata: record.metadata,
            tags: record.tags,
        }
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

pub use crate::api::btql::DatasetRecord;

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

use crate::api::btql::{BTQLQuery, BTQLQueryInner};
use crate::api::datasets::DatasetRegisterRequest;

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
pub struct DatasetBuilder {
    api: Arc<dyn DatasetApi>,
    lifecycle: Arc<dyn DeliveryControl>,
    scope: CredentialScope,
    project_name: Option<String>,
    dataset_name: Option<String>,
    description: Option<String>,
    metadata: Option<Map<String, Value>>,
}

impl Clone for DatasetBuilder {
    fn clone(&self) -> Self {
        Self {
            api: self.api.clone(),
            lifecycle: self.lifecycle.clone(),
            scope: self.scope.clone(),
            project_name: self.project_name.clone(),
            dataset_name: self.dataset_name.clone(),
            description: self.description.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

impl DatasetBuilder {
    /// Create a new DatasetBuilder (internal use).
    pub(crate) fn new(
        api: Arc<dyn DatasetApi>,
        lifecycle: Arc<dyn DeliveryControl>,
        scope: CredentialScope,
    ) -> Self {
        Self {
            api,
            lifecycle,
            scope,
            project_name: None,
            dataset_name: None,
            description: None,
            metadata: None,
        }
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
    pub fn build(self) -> std::result::Result<Dataset, DatasetBuilderError> {
        let project_name = self
            .project_name
            .ok_or(DatasetBuilderError::MissingProjectName)?;
        let dataset_name = self
            .dataset_name
            .ok_or(DatasetBuilderError::MissingDatasetName)?;

        Ok(Dataset {
            api: self.api,
            lifecycle: self.lifecycle,
            scope: self.scope,
            project_name,
            dataset_name,
            description: self.description,
            metadata: self.metadata,
            lazy_metadata: Arc::new(OnceCell::new()),
        })
    }
}

// ============================================================================
// Dataset
// ============================================================================

/// A dataset for storing test/evaluation data.
///
/// Datasets are lazily registered with the API on first use.
pub struct Dataset {
    api: Arc<dyn DatasetApi>,
    lifecycle: Arc<dyn DeliveryControl>,
    scope: CredentialScope,
    project_name: String,
    dataset_name: String,
    description: Option<String>,
    metadata: Option<Map<String, Value>>,
    lazy_metadata: Arc<OnceCell<DatasetMetadata>>,
}

impl Clone for Dataset {
    fn clone(&self) -> Self {
        Self {
            api: self.api.clone(),
            lifecycle: self.lifecycle.clone(),
            scope: self.scope.clone(),
            project_name: self.project_name.clone(),
            dataset_name: self.dataset_name.clone(),
            description: self.description.clone(),
            metadata: self.metadata.clone(),
            lazy_metadata: self.lazy_metadata.clone(),
        }
    }
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
                let request = DatasetRegisterRequest {
                    project_name: self.project_name.clone(),
                    org_name: self.scope.org_name().unwrap_or_default().to_owned(),
                    dataset_name: self.dataset_name.clone(),
                    description: self.description.clone(),
                    metadata: self.metadata.clone(),
                };
                let response = self.api.register_dataset(&self.scope, request).await?;
                Ok(DatasetMetadata {
                    dataset_id: response.dataset.id,
                    project_id: response.project.id,
                })
            })
            .await
    }

    /// Get the dataset ID (triggers registration if needed).
    pub async fn dataset_id(&self) -> Result<String> {
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
    pub async fn insert(&self, record: DatasetInsert) -> Result<String> {
        let dataset_id = self.ensure_registered().await?.dataset_id.clone();
        let id = Uuid::new_v4().to_string();
        self.api
            .write_dataset_record(
                &self.scope,
                DatasetWrite::Insert {
                    dataset_id,
                    id: id.clone(),
                    fields: record.into(),
                },
            )
            .await?;
        Ok(id)
    }

    /// Merge supplied fields into a record, returning after API acknowledgement.
    pub async fn update(&self, id: &str, record: DatasetInsert) -> Result<()> {
        let dataset_id = self.ensure_registered().await?.dataset_id.clone();
        self.api
            .write_dataset_record(
                &self.scope,
                DatasetWrite::Update {
                    dataset_id,
                    id: id.to_owned(),
                    fields: record.into(),
                },
            )
            .await
    }

    /// Delete a record, returning after API acknowledgement.
    pub async fn delete(&self, id: &str) -> Result<()> {
        let dataset_id = self.ensure_registered().await?.dataset_id.clone();
        self.api
            .write_dataset_record(
                &self.scope,
                DatasetWrite::Delete {
                    dataset_id,
                    id: id.to_owned(),
                },
            )
            .await
    }

    /// Fetch records with pagination.
    ///
    /// Returns an iterator that fetches records in batches.
    pub async fn fetch(&self, batch_size: Option<usize>) -> Result<DatasetIterator> {
        let metadata = self.ensure_registered().await?;
        Ok(DatasetIterator::new(
            self.api.clone(),
            self.scope.clone(),
            metadata.dataset_id.clone(),
            batch_size.unwrap_or(1000).max(1),
        ))
    }

    /// Get dataset summary statistics.
    pub async fn summarize(&self) -> Result<DatasetSummary> {
        let metadata = self.ensure_registered().await?;

        // Fetch summary from API
        let response = self
            .api
            .fetch_dataset_summary(&self.scope, &metadata.dataset_id)
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
    }

    /// Flush the client's shared trace queue. Dataset mutations are already
    /// acknowledged when their individual operation completes.
    pub fn flush(&self) -> crate::Completion<crate::FlushReport, crate::FlushError> {
        self.lifecycle.flush()
    }
}

// ============================================================================
// DatasetIterator
// ============================================================================

/// Iterator for fetching dataset records with pagination.
///
/// ```no_run
/// use braintrust_sdk_rust::{BraintrustError, Dataset};
/// # async fn read(dataset: &Dataset) -> Result<(), BraintrustError> {
/// let mut records = dataset.fetch(Some(100)).await?;
/// while let Some(record) = records.next().await? {
///     println!("{}", record.id);
/// }
/// # Ok(())
/// # }
/// ```
pub struct DatasetIterator {
    fetcher: Arc<dyn DatasetApi>,
    scope: CredentialScope,
    dataset_id: String,
    batch_size: usize,
    cursor: Option<String>,
    buffer: VecDeque<DatasetRecord>,
    done: bool,
}

impl DatasetIterator {
    fn new(
        fetcher: Arc<dyn DatasetApi>,
        scope: CredentialScope,
        dataset_id: String,
        batch_size: usize,
    ) -> Self {
        Self {
            fetcher,
            scope,
            dataset_id,
            batch_size,
            cursor: None,
            buffer: VecDeque::new(),
            done: false,
        }
    }

    /// Get the next record, fetching a new batch if needed.
    pub async fn next(&mut self) -> Result<Option<DatasetRecord>> {
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
            let response = self
                .fetcher
                .fetch_dataset_records(&self.scope, query)
                .await?;
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

    /// Collect all remaining records, preserving any page failure as an error.
    pub async fn collect(mut self) -> Result<Vec<DatasetRecord>> {
        let mut records = Vec::new();
        while let Some(record) = self.next().await? {
            records.push(record);
        }
        Ok(records)
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
