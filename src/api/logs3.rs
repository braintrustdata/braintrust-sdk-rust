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
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowInputRow {
    /// Key identifying fields extracted from the row (experiment_id, dataset_id, etc.)
    pub(crate) object_ids: serde_json::Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) is_delete: Option<bool>,
    pub(crate) input_row: Logs3OverflowInputRowMeta,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowInputRowMeta {
    pub(crate) byte_size: usize,
}

/// Reference sent to POST logs3 after a successful overflow upload.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowRequest {
    pub(crate) rows: Logs3OverflowReference,
    pub(crate) api_version: u8,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowReference {
    #[serde(rename = "type")]
    pub(crate) reference_type: &'static str,
    pub(crate) key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct VersionInfo {
    pub(crate) logs3_payload_max_bytes: Option<usize>,
}
/// Explicit request for an overflow grant. Uploading the object is a separate operation.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowUploadRequest {
    pub(crate) content_type: String,
    pub(crate) size_bytes: usize,
    pub(crate) rows: Vec<Logs3OverflowInputRow>,
}
/// Input for a single logs3 insertion. Rows use the existing ingestion wire format.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct InsertRows {
    pub(crate) rows: Vec<Value>,
    pub(crate) api_version: u8,
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
    /// Set the object ids.
    pub fn set_object_ids(&mut self, value: serde_json::Map<String, Value>) -> &mut Self {
        self.object_ids = value;
        self
    }
    /// Return the is delete.
    pub fn is_delete(&self) -> Option<&bool> {
        self.is_delete.as_ref()
    }
    /// Set the is delete.
    pub fn set_is_delete(&mut self, value: Option<bool>) -> &mut Self {
        self.is_delete = value;
        self
    }
    /// Return the input row.
    pub fn input_row(&self) -> &Logs3OverflowInputRowMeta {
        &self.input_row
    }
    /// Set the input row.
    pub fn set_input_row(&mut self, value: Logs3OverflowInputRowMeta) -> &mut Self {
        self.input_row = value;
        self
    }
}

impl Logs3OverflowInputRowMeta {
    /// Return the byte size.
    pub fn byte_size(&self) -> usize {
        self.byte_size
    }
    /// Set the byte size.
    pub fn set_byte_size(&mut self, value: usize) -> &mut Self {
        self.byte_size = value;
        self
    }
}

impl Logs3OverflowRequest {
    /// Return the rows.
    pub fn rows(&self) -> &Logs3OverflowReference {
        &self.rows
    }
    /// Set the rows.
    pub fn set_rows(&mut self, value: Logs3OverflowReference) -> &mut Self {
        self.rows = value;
        self
    }
    /// Return the api version.
    pub fn api_version(&self) -> u8 {
        self.api_version
    }
    /// Set the api version.
    pub fn set_api_version(&mut self, value: u8) -> &mut Self {
        self.api_version = value;
        self
    }
}

impl Logs3OverflowReference {
    /// Return the reference type.
    pub fn reference_type(&self) -> &&'static str {
        &self.reference_type
    }
    /// Set the reference type.
    pub fn set_reference_type(&mut self, value: &'static str) -> &mut Self {
        self.reference_type = value;
        self
    }
    /// Return the key.
    pub fn key(&self) -> &str {
        &self.key
    }
    /// Set the key.
    pub fn set_key(&mut self, value: impl Into<String>) -> &mut Self {
        self.key = value.into();
        self
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
    /// Set the content type.
    pub fn set_content_type(&mut self, value: impl Into<String>) -> &mut Self {
        self.content_type = value.into();
        self
    }
    /// Return the size bytes.
    pub fn size_bytes(&self) -> usize {
        self.size_bytes
    }
    /// Set the size bytes.
    pub fn set_size_bytes(&mut self, value: usize) -> &mut Self {
        self.size_bytes = value;
        self
    }
    /// Return the rows.
    pub fn rows(&self) -> &[Logs3OverflowInputRow] {
        &self.rows
    }
    /// Set the rows.
    pub fn set_rows(&mut self, value: Vec<Logs3OverflowInputRow>) -> &mut Self {
        self.rows = value;
        self
    }
}

impl InsertRows {
    /// Return the rows.
    pub fn rows(&self) -> &[Value] {
        &self.rows
    }
    /// Set the rows.
    pub fn set_rows(&mut self, value: Vec<Value>) -> &mut Self {
        self.rows = value;
        self
    }
    /// Return the api version.
    pub fn api_version(&self) -> u8 {
        self.api_version
    }
    /// Set the api version.
    pub fn set_api_version(&mut self, value: u8) -> &mut Self {
        self.api_version = value;
        self
    }
}
