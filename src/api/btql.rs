//! BTQL dataset readback, independent of SDK ingestion.
use super::{Client, Error, Plane};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct BTQLQuery {
    pub(crate) query: BTQLQueryInner,
}

/// Inner BTQL query structure.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct BTQLQueryInner {
    /// Dataset reference: dataset('id')
    pub(crate) from: String,
    /// Maximum records per page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) limit: Option<usize>,
    /// Pagination cursor token.
    #[serde(skip_serializing_if = "Option::is_none")]
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

#[derive(Clone)]
pub struct Btql {
    pub(crate) client: Client,
}
impl Btql {
    /// Execute one dataset page request; no visibility polling or pagination loop.
    pub async fn query_dataset_records(&self, query: BTQLQuery) -> Result<BTQLResponse, Error> {
        self.client.post_json(Plane::Data, "btql", &query).await
    }
}

impl BTQLQuery {
    /// Construct a dataset query from its source expression.
    pub fn new(from: impl Into<String>) -> Self {
        Self {
            query: BTQLQueryInner::new(from),
        }
    }
}
impl BTQLQueryInner {
    /// Construct the query body with no page limit or cursor.
    pub fn new(from: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            limit: None,
            cursor: None,
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

impl BTQLQuery {
    /// Return the query.
    pub fn query(&self) -> &BTQLQueryInner {
        &self.query
    }
    /// Set the query.
    pub fn set_query(&mut self, value: BTQLQueryInner) -> &mut Self {
        self.query = value;
        self
    }
}

impl BTQLQueryInner {
    /// Return the from.
    pub fn from(&self) -> &str {
        &self.from
    }
    /// Set the from.
    pub fn set_from(&mut self, value: impl Into<String>) -> &mut Self {
        self.from = value.into();
        self
    }
    /// Return the limit.
    pub fn limit(&self) -> Option<&usize> {
        self.limit.as_ref()
    }
    /// Set the limit.
    pub fn set_limit(&mut self, value: Option<usize>) -> &mut Self {
        self.limit = value;
        self
    }
    /// Return the cursor.
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }
    /// Set the cursor.
    pub fn set_cursor(&mut self, value: Option<String>) -> &mut Self {
        self.cursor = value;
        self
    }
}

impl BTQLResponse {
    /// Return the data.
    pub fn data(&self) -> &[DatasetRecord] {
        &self.data
    }
    /// Return the cursor.
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }
}
