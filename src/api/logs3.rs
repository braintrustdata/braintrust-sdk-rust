//! Direct logs3 requests: no batching, splitting, retries or implicit overflow.
use super::{Client, Error, Plane};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Response from POST logs3/overflow — provides a signed URL to upload the payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Logs3OverflowUpload {
    pub method: String,
    pub signed_url: String,
    pub headers: Option<HashMap<String, String>>,
    pub fields: Option<HashMap<String, String>>,
    pub key: String,
}

/// A single row's overflow metadata, sent when requesting the overflow upload URL.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowInputRow {
    /// Key identifying fields extracted from the row (experiment_id, dataset_id, etc.)
    pub object_ids: serde_json::Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_delete: Option<bool>,
    pub input_row: Logs3OverflowInputRowMeta,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowInputRowMeta {
    pub byte_size: usize,
}

/// Reference sent to POST logs3 after a successful overflow upload.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowRequest {
    pub rows: Logs3OverflowReference,
    pub api_version: u8,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowReference {
    #[serde(rename = "type")]
    pub reference_type: &'static str,
    pub key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct VersionInfo {
    pub logs3_payload_max_bytes: Option<usize>,
}
/// Explicit request for an overflow grant. Uploading the object is a separate operation.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowUploadRequest {
    pub content_type: String,
    pub size_bytes: usize,
    pub rows: Vec<Logs3OverflowInputRow>,
}
/// Input for a single logs3 insertion. Rows use the existing ingestion wire format.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct InsertRows {
    pub rows: Vec<Value>,
    pub api_version: u8,
}
impl InsertRows {
    pub fn new(rows: Vec<Value>) -> Self {
        Self {
            rows,
            api_version: 2,
        }
    }
}
#[derive(Clone)]
pub struct Logs3 {
    pub(crate) client: Client,
}
impl Logs3 {
    pub async fn version(&self) -> Result<VersionInfo, Error> {
        self.client.get_json(Plane::Data, "version", &[]).await
    }
    /// Submit exactly this body once, accepting any successful HTTP status.
    pub async fn post_raw(&self, payload: Vec<u8>) -> Result<(), Error> {
        self.client.post_bytes("logs3", payload).await
    }
    pub async fn insert(&self, request: InsertRows) -> Result<(), Error> {
        self.client.post_empty(Plane::Data, "logs3", &request).await
    }
    pub async fn request_overflow_upload(
        &self,
        request: Logs3OverflowUploadRequest,
    ) -> Result<Logs3OverflowUpload, Error> {
        self.client
            .post_json(Plane::Data, "logs3/overflow", &request)
            .await
    }
    pub async fn commit_overflow(&self, request: Logs3OverflowRequest) -> Result<(), Error> {
        self.client.post_empty(Plane::Data, "logs3", &request).await
    }
}

impl Logs3OverflowInputRow {
    /// Construct overflow metadata for one row.
    pub fn new(object_ids: serde_json::Map<String, Value>, byte_size: usize) -> Self {
        Self {
            object_ids,
            is_delete: None,
            input_row: Logs3OverflowInputRowMeta::new(byte_size),
        }
    }
}
impl Logs3OverflowInputRowMeta {
    /// Construct row size metadata in bytes.
    pub fn new(byte_size: usize) -> Self {
        Self { byte_size }
    }
}
impl Logs3OverflowReference {
    /// Construct a reference to an uploaded overflow object.
    pub fn new(reference_type: &'static str, key: impl Into<String>) -> Self {
        Self {
            reference_type,
            key: key.into(),
        }
    }
}
impl Logs3OverflowRequest {
    /// Construct an overflow insertion with the current logs API version.
    pub fn new(rows: Logs3OverflowReference) -> Self {
        Self {
            rows,
            api_version: 2,
        }
    }
}
impl Logs3OverflowUploadRequest {
    /// Construct a request for an overflow upload grant.
    pub fn new(
        content_type: impl Into<String>,
        size_bytes: usize,
        rows: Vec<Logs3OverflowInputRow>,
    ) -> Self {
        Self {
            content_type: content_type.into(),
            size_bytes,
            rows,
        }
    }
}
