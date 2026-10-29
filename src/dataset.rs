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
//!     .build();
//!
//! let id = dataset.insert(
//!     DatasetInsert::builder()
//!         .input(json!({"question": "What is 2+2?"}))
//!         .expected(json!({"answer": "4"}))
//!         .build()
//! ).await?;
//! ```

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::OnceCell;
use uuid::Uuid;

pub use crate::api::datasets::DatasetFields;
use crate::api::datasets::DatasetWrite;
use crate::error::Result;
use crate::internal::ports::DeliveryControl;
use crate::internal::resources::ResourceTransport;
use crate::internal::session::CredentialScope;

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
    api: ResourceTransport,
    #[builder(start_fn)]
    lifecycle: Arc<dyn DeliveryControl>,
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
                DatasetWrite::insert(dataset_id, id.clone(), record.into()),
            )
            .await?;
        Ok(id)
    }

    /// Merge supplied fields into a record, returning after API acknowledgement.
    /// Omitted fields remain unchanged; explicit JSON null values are sent as null.
    ///
    /// ```no_run
    /// use braintrust_sdk_rust::{Dataset, DatasetFields};
    /// # async fn update(dataset: &Dataset) -> braintrust_sdk_rust::Result<()> {
    /// dataset.update("record-id", DatasetFields::builder().expected(4).build()).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn update(&self, id: &str, fields: DatasetFields) -> Result<()> {
        let dataset_id = self.ensure_registered().await?.dataset_id.clone();
        self.api
            .write_dataset_record(&self.scope, DatasetWrite::update(dataset_id, id, fields))
            .await
    }

    /// Delete a record, returning after API acknowledgement.
    pub async fn delete(&self, id: &str) -> Result<()> {
        let dataset_id = self.ensure_registered().await?.dataset_id.clone();
        self.api
            .write_dataset_record(&self.scope, DatasetWrite::delete(dataset_id, id))
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
///     println!("{}", record.id());
/// }
/// # Ok(())
/// # }
/// ```
pub struct DatasetIterator {
    fetcher: ResourceTransport,
    scope: CredentialScope,
    dataset_id: String,
    batch_size: usize,
    cursor: Option<String>,
    buffer: VecDeque<DatasetRecord>,
    done: bool,
}

impl DatasetIterator {
    fn new(
        fetcher: ResourceTransport,
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
