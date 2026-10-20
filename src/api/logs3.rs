//! Direct logs3 requests: no batching, splitting, retries or implicit overflow.
use super::{Client, Error, Plane};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Response from POST logs3/overflow — provides a signed URL to upload the payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Logs3OverflowUpload {
    pub method: String,
    pub signed_url: String,
    pub headers: Option<HashMap<String, String>>,
    pub fields: Option<HashMap<String, String>>,
    pub key: String,
}

/// A single row's overflow metadata, sent when requesting the overflow upload URL.
#[derive(Debug, Clone, Serialize)]
pub struct Logs3OverflowInputRow {
    /// Key identifying fields extracted from the row (experiment_id, dataset_id, etc.)
    pub object_ids: serde_json::Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_delete: Option<bool>,
    pub input_row: Logs3OverflowInputRowMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct Logs3OverflowInputRowMeta {
    pub byte_size: usize,
}

/// Reference sent to POST logs3 after a successful overflow upload.
#[derive(Debug, Clone, Serialize)]
pub struct Logs3OverflowRequest {
    pub rows: Logs3OverflowReference,
    pub api_version: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct Logs3OverflowReference {
    #[serde(rename = "type")]
    pub reference_type: &'static str,
    pub key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionInfo {
    pub logs3_payload_max_bytes: Option<usize>,
}
/// Explicit request for an overflow grant. Uploading the object is a separate operation.
#[derive(Debug, Clone, Serialize)]
pub struct Logs3OverflowUploadRequest {
    pub content_type: String,
    pub size_bytes: usize,
    pub rows: Vec<Logs3OverflowInputRow>,
}
/// Input for a single logs3 insertion. Rows use the existing ingestion wire format.
#[derive(Debug, Clone, Serialize)]
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
