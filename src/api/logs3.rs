//! Raw logs3 endpoints.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::client::ApiClient;

pub const LOGS_API_VERSION: u8 = 2;

/// Response from POST logs3/overflow — provides a signed URL to upload the payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Logs3OverflowUpload {
    method: String,
    signed_url: String,
    headers: Option<HashMap<String, String>>,
    fields: Option<HashMap<String, String>>,
    key: String,
}

impl Logs3OverflowUpload {
    api_getters! {
        str method;
        str signed_url;
        option_hash_map headers: String;
        option_hash_map fields: String;
        str key;
    }
}

/// A single row's overflow metadata, sent when requesting the overflow upload URL.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowInputRow {
    /// Key identifying fields extracted from the row (experiment_id, dataset_id, etc.)
    pub object_ids: serde_json::Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_delete: Option<bool>,
    pub input_row: Logs3OverflowInputRowMeta,
}

impl Logs3OverflowInputRow {
    api_getters! {
        map object_ids;
        option_bool is_delete;
        ref input_row: Logs3OverflowInputRowMeta;
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowInputRowMeta {
    pub byte_size: usize,
}

impl Logs3OverflowInputRowMeta {
    api_getters! {
        usize byte_size;
    }
}

/// Reference sent to POST logs3 after a successful overflow upload.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowRequest {
    pub rows: Logs3OverflowReference,
    pub api_version: u8,
}

impl Logs3OverflowRequest {
    api_getters! {
        ref rows: Logs3OverflowReference;
        u8 api_version;
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowReference {
    #[serde(rename = "type")]
    pub reference_type: String,
    pub key: String,
}

impl Logs3OverflowReference {
    api_getters! {
        str reference_type;
        str key;
    }
}

/// The object ID keys that identify a row's destination, used for overflow metadata.
/// Matches the TypeScript SDK's OBJECT_ID_KEYS.
///
/// Note: `function_data` is included to match the TypeScript SDK's key set, but is not
/// yet a field on `Logs3Row`. It will be populated once a `FunctionLogs` destination
/// type is added in a future release.
#[allow(dead_code)]
pub const OBJECT_ID_KEYS: &[&str] = &[
    "experiment_id",
    "dataset_id",
    "prompt_session_id",
    "project_id",
    "log_id",
    "function_data",
];

/// Client for logs3 API operations.
#[derive(Clone, Debug)]
pub struct Logs3Client {
    api: ApiClient,
}

impl Logs3Client {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    pub async fn version(&self) -> crate::Result<VersionInfo> {
        self.api.get_json("version", "version response").await
    }

    pub async fn post_raw(&self, payload: Vec<u8>) -> crate::Result<()> {
        self.api.post_bytes("logs3", payload).await
    }

    pub async fn request_overflow_upload(
        &self,
        request: Logs3OverflowUploadRequest,
    ) -> crate::Result<Logs3OverflowUpload> {
        self.api
            .post_json("logs3/overflow", &request, "logs3 overflow response")
            .await
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionInfo {
    #[serde(default)]
    logs3_payload_max_bytes: Option<usize>,
}

impl VersionInfo {
    api_getters! {
        option_usize logs3_payload_max_bytes;
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct Logs3OverflowUploadRequest {
    pub content_type: String,
    pub size_bytes: usize,
    pub rows: Vec<Logs3OverflowInputRow>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::api::test_support::{
        api_client_with_urls, auth_header, json_body, request, request_with_body,
    };

    #[tokio::test]
    async fn logs3_endpoints_use_api_url_and_org_header() {
        let api_server = MockServer::start().await;
        let app_server = MockServer::start().await;
        let api = api_client_with_urls(api_server.uri(), app_server.uri(), "Org");

        Mock::given(method("GET"))
            .and(path("/version"))
            .and(header("authorization", "Bearer token"))
            .and(header("x-bt-org-name", "Org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "logs3_payload_max_bytes": 1024
            })))
            .mount(&api_server)
            .await;

        let version = api.logs3().version().await.expect("version");
        assert_eq!(version.logs3_payload_max_bytes(), Some(1024));

        Mock::given(method("POST"))
            .and(path("/logs3"))
            .and(header("authorization", "Bearer token"))
            .and(header("x-bt-org-name", "Org"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&api_server)
            .await;

        api.logs3()
            .post_raw(br#"{"rows":[],"api_version":2}"#.to_vec())
            .await
            .expect("logs3 post");

        Mock::given(method("POST"))
            .and(path("/logs3/overflow"))
            .and(header("authorization", "Bearer token"))
            .and(header("x-bt-org-name", "Org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "method": "PUT",
                "signedUrl": "http://signed.example/upload",
                "key": "overflow-key"
            })))
            .mount(&api_server)
            .await;

        let row = Logs3OverflowInputRow {
            object_ids: serde_json::Map::new(),
            input_row: Logs3OverflowInputRowMeta { byte_size: 10 },
            ..Default::default()
        };
        let upload = api
            .logs3()
            .request_overflow_upload(Logs3OverflowUploadRequest {
                content_type: "application/json".to_string(),
                size_bytes: 10,
                rows: vec![row],
            })
            .await
            .expect("overflow upload");
        assert_eq!(upload.key(), "overflow-key");

        let api_requests = api_server.received_requests().await.expect("requests");
        let version_request = request(&api_requests, "GET", "/version");
        assert_eq!(auth_header(version_request), "Bearer token");

        let logs_request = request_with_body(&api_requests, "POST", "/logs3");
        assert_eq!(json_body(logs_request)["api_version"], 2);

        let overflow_request = request_with_body(&api_requests, "POST", "/logs3/overflow");
        assert_eq!(
            json_body(overflow_request)["content_type"],
            "application/json"
        );
        assert_eq!(json_body(overflow_request)["size_bytes"], 10);

        let app_requests = app_server.received_requests().await.expect("requests");
        assert!(app_requests.is_empty());
    }
}
