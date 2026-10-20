//! BTQL dataset readback, independent of SDK ingestion.
use super::{Client, Error, Plane, Transport};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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

/// BTQL query for fetching dataset records.
#[derive(Debug, Clone, Serialize)]
pub struct BTQLQuery {
    pub query: BTQLQueryInner,
    /// Optional immutable dataset snapshot version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Inner BTQL query structure.
#[derive(Debug, Clone, Serialize)]
pub struct BTQLQueryInner {
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
pub struct BTQLResponse {
    /// Records in JSONL format.
    #[serde(default)]
    pub data: Vec<DatasetRecord>,
    /// Cursor for next page (None if last page).
    pub cursor: Option<String>,
}

#[derive(Clone)]
pub struct Btql {
    pub(crate) client: Client,
}
impl Btql {
    /// Read one dataset page without pagination loops or visibility polling.
    /// Submission is immediate; dropping the token does not cancel accepted work.
    pub fn query_dataset_records(
        &self,
        query: BTQLQuery,
    ) -> crate::Completion<BTQLResponse, Error> {
        self.client
            .submit(move |transport| async move { transport.query_dataset_records(query).await })
    }
}

impl Transport {
    /// Execute one dataset page request; no visibility polling or pagination loop.
    pub(crate) async fn query_dataset_records(
        &self,
        query: BTQLQuery,
    ) -> Result<BTQLResponse, Error> {
        self.post_json(Plane::Data, "btql", &query).await
    }
}
