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
    pub(crate) method: String,
    pub(crate) signed_url: String,
    pub(crate) headers: Option<HashMap<String, String>>,
    pub(crate) fields: Option<HashMap<String, String>>,
    pub(crate) key: String,
}

/// A single row's overflow metadata, sent when requesting the overflow upload URL.
#[derive(bon::Builder, Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowInputRow {
    /// Key identifying fields extracted from the row (experiment_id, dataset_id, etc.)
    pub(crate) object_ids: serde_json::Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) is_delete: Option<bool>,
    pub(crate) input_row: Logs3OverflowInputRowMeta,
}

#[derive(bon::Builder, Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowInputRowMeta {
    pub(crate) byte_size: usize,
}

/// Reference sent to POST logs3 after a successful overflow upload.
#[derive(bon::Builder, Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowRequest {
    pub(crate) rows: Logs3OverflowReference,
    #[builder(default = 2)]
    pub(crate) api_version: u8,
}

#[derive(bon::Builder, Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowReference {
    #[serde(rename = "type")]
    pub(crate) reference_type: &'static str,
    #[builder(into)]
    pub(crate) key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct VersionInfo {
    pub(crate) logs3_payload_max_bytes: Option<usize>,
}
/// Explicit request for an overflow grant. Uploading the object is a separate operation.
#[derive(bon::Builder, Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowUploadRequest {
    #[builder(into)]
    pub(crate) content_type: String,
    pub(crate) size_bytes: usize,
    pub(crate) rows: Vec<Logs3OverflowInputRow>,
}
/// Input for a single logs3 insertion. Rows use the existing ingestion wire format.
#[derive(bon::Builder, Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct InsertRows {
    pub(crate) rows: Vec<Value>,
    #[builder(default = 2)]
    pub(crate) api_version: u8,
}
impl InsertRows {
    pub fn new(rows: Vec<Value>) -> Self {
        Self::builder().rows(rows).build()
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
        Self::builder().rows(rows).build()
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

impl Logs3OverflowUpload {
    /// Return the method.
    pub fn method(&self) -> &str {
        &self.method
    }
    /// Return the signed url.
    pub fn signed_url(&self) -> &str {
        &self.signed_url
    }
    /// Return the headers.
    pub fn headers(&self) -> Option<&HashMap<String, String>> {
        self.headers.as_ref()
    }
    /// Return the fields.
    pub fn fields(&self) -> Option<&HashMap<String, String>> {
        self.fields.as_ref()
    }
    /// Return the key.
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl Logs3OverflowInputRow {
    /// Return the object ids.
    pub fn object_ids(&self) -> &serde_json::Map<String, Value> {
        &self.object_ids
    }
    /// Return the is delete.
    pub fn is_delete(&self) -> Option<&bool> {
        self.is_delete.as_ref()
    }
    /// Return the input row.
    pub fn input_row(&self) -> &Logs3OverflowInputRowMeta {
        &self.input_row
    }
}

impl Logs3OverflowInputRowMeta {
    /// Return the byte size.
    pub fn byte_size(&self) -> usize {
        self.byte_size
    }
}

impl Logs3OverflowRequest {
    /// Return the rows.
    pub fn rows(&self) -> &Logs3OverflowReference {
        &self.rows
    }
    /// Return the api version.
    pub fn api_version(&self) -> u8 {
        self.api_version
    }
}

impl Logs3OverflowReference {
    /// Return the reference type.
    pub fn reference_type(&self) -> &&'static str {
        &self.reference_type
    }
    /// Return the key.
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl VersionInfo {
    /// Return the logs3 payload max bytes.
    pub fn logs3_payload_max_bytes(&self) -> Option<&usize> {
        self.logs3_payload_max_bytes.as_ref()
    }
}

impl Logs3OverflowUploadRequest {
    /// Return the content type.
    pub fn content_type(&self) -> &str {
        &self.content_type
    }
    /// Return the size bytes.
    pub fn size_bytes(&self) -> usize {
        self.size_bytes
    }
    /// Return the rows.
    pub fn rows(&self) -> &[Logs3OverflowInputRow] {
        &self.rows
    }
}

impl InsertRows {
    /// Return the rows.
    pub fn rows(&self) -> &[Value] {
        &self.rows
    }
    /// Return the api version.
    pub fn api_version(&self) -> u8 {
        self.api_version
    }
}
