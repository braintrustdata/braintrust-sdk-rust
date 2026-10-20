//! Direct logs3 requests: no batching, splitting, retries or implicit overflow.
use super::{Client, Error, Plane, Transport};
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
    /// Fetch ingestion capabilities once.
    /// Submission is immediate; dropping the token does not cancel accepted work.
    pub fn version(&self) -> crate::Completion<VersionInfo, Error> {
        self.client
            .submit(move |transport| async move { transport.logs3_version().await })
    }
    /// Submit exactly this body once, accepting any successful HTTP status.
    /// Submission is immediate; dropping the token does not cancel accepted work.
    pub fn post_raw(&self, payload: Vec<u8>) -> crate::Completion<(), Error> {
        self.client
            .submit(move |transport| async move { transport.logs3_post_raw(payload).await })
    }
    /// Submit exactly these rows once, without batching or retries.
    /// Submission is immediate; dropping the token does not cancel accepted work.
    pub fn insert(&self, request: InsertRows) -> crate::Completion<(), Error> {
        self.client
            .submit(move |transport| async move { transport.logs3_insert(request).await })
    }
    /// Request an overflow grant without uploading its object or committing it.
    /// Submission is immediate; dropping the token does not cancel accepted work.
    pub fn request_overflow_upload(
        &self,
        request: Logs3OverflowUploadRequest,
    ) -> crate::Completion<Logs3OverflowUpload, Error> {
        self.client.submit(move |transport| async move {
            transport.logs3_request_overflow_upload(request).await
        })
    }
    /// Submit an explicit overflow reference once.
    /// Submission is immediate; dropping the token does not cancel accepted work.
    pub fn commit_overflow(&self, request: Logs3OverflowRequest) -> crate::Completion<(), Error> {
        self.client
            .submit(move |transport| async move { transport.logs3_commit_overflow(request).await })
    }
}

impl Transport {
    pub(crate) async fn logs3_version(&self) -> Result<VersionInfo, Error> {
        self.get_json(Plane::Data, "version", &[]).await
    }
    /// Submit exactly this body once, accepting any successful HTTP status.
    pub(crate) async fn logs3_post_raw(&self, payload: Vec<u8>) -> Result<(), Error> {
        self.post_bytes("logs3", payload).await
    }
    pub(crate) async fn logs3_insert(&self, request: InsertRows) -> Result<(), Error> {
        self.post_empty(Plane::Data, "logs3", &request).await
    }
    pub(crate) async fn logs3_request_overflow_upload(
        &self,
        request: Logs3OverflowUploadRequest,
    ) -> Result<Logs3OverflowUpload, Error> {
        self.post_json(Plane::Data, "logs3/overflow", &request)
            .await
    }
    pub(crate) async fn logs3_commit_overflow(
        &self,
        request: Logs3OverflowRequest,
    ) -> Result<(), Error> {
        self.post_empty(Plane::Data, "logs3", &request).await
    }
}
