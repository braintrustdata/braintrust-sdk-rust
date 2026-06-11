//! Attachment endpoints.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::api::client::ApiClient;

/// Client for attachment API operations.
#[derive(Clone, Debug)]
pub struct AttachmentsClient {
    api: ApiClient,
}

impl AttachmentsClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    /// Creates an attachment upload through `/attachment`.
    pub async fn upload(
        &self,
        request: UploadAttachmentRequest,
    ) -> crate::Result<UploadAttachmentResponse> {
        self.api
            .post_json("attachment", &request, "attachment response")
            .await
    }

    /// Backwards-compatible alias for [`Self::upload`].
    pub async fn create(
        &self,
        request: UploadAttachmentRequest,
    ) -> crate::Result<UploadAttachmentResponse> {
        self.upload(request).await
    }

    /// Gets an attachment download URL through `/attachment`.
    pub async fn get(&self, request: GetAttachmentRequest) -> crate::Result<GetAttachmentResponse> {
        self.api
            .get_json_with_query("attachment", &request, "attachment get response")
            .await
    }

    /// Updates attachment status through `/attachment/status`.
    pub async fn put_status(
        &self,
        request: PutAttachmentStatusRequest,
    ) -> crate::Result<PutAttachmentStatusResponse> {
        self.api
            .post_json("attachment/status", &request, "attachment status response")
            .await
    }
}

/// Request body for creating a Braintrust attachment upload.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct UploadAttachmentRequest {
    pub filename: String,
    pub content_type: String,
    pub org_id: String,
    pub key: String,
}

/// Query parameters for getting a Braintrust or external attachment.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct GetAttachmentRequest {
    pub filename: String,
    pub content_type: String,
    pub org_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Request body for updating attachment upload status.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct PutAttachmentStatusRequest {
    pub status: AttachmentStatus,
    pub org_id: String,
    pub key: String,
}

/// Upload status for an attachment.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UploadStatus {
    Uploading,
    Done,
    Error,
}

/// Attachment upload status.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct AttachmentStatus {
    pub upload_status: UploadStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Response returned by `/attachment`.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct UploadAttachmentResponse {
    #[serde(rename = "signedUrl")]
    signed_url: String,
    headers: BTreeMap<String, String>,
}

impl UploadAttachmentResponse {
    api_getters! {
        str signed_url;
    }

    /// Returns headers that must be sent with the signed upload request.
    pub fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
    }
}

/// Response returned by `GET /attachment`.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct GetAttachmentResponse {
    #[serde(rename = "downloadUrl")]
    download_url: String,
    #[serde(rename = "embedUrl")]
    embed_url: String,
    #[serde(rename = "contentLength", default)]
    content_length: Option<u64>,
    status: AttachmentStatus,
}

impl GetAttachmentResponse {
    api_getters! {
        str download_url;
        str embed_url;
        option_u64 content_length;
        ref status: AttachmentStatus;
    }
}

/// Response returned by `POST /attachment/status`.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct PutAttachmentStatusResponse {
    status: String,
}

impl PutAttachmentStatusResponse {
    api_getters! {
        str status;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::api::test_support::{
        api_client_with_urls, assert_query_value, auth_header, json_body, request,
        request_with_body,
    };

    #[tokio::test]
    async fn attachment_routes_use_api_url_methods_auth_org_and_json() {
        let api_server = MockServer::start().await;
        let app_server = MockServer::start().await;
        let api = api_client_with_urls(api_server.uri(), app_server.uri(), "Org");

        Mock::given(method("POST"))
            .and(path("/attachment"))
            .and(header("authorization", "Bearer token"))
            .and(header("x-bt-org-name", "Org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "signedUrl": "https://example.test/upload",
                "headers": {
                    "Content-Type": "text/plain",
                    "If-None-Match": "*"
                }
            })))
            .mount(&api_server)
            .await;

        assert_eq!(
            api.attachments()
                .upload(UploadAttachmentRequest {
                    filename: "example.txt".to_string(),
                    content_type: "text/plain".to_string(),
                    org_id: "org-id".to_string(),
                    key: "attachment-key".to_string(),
                })
                .await
                .expect("create")
                .signed_url(),
            "https://example.test/upload"
        );

        Mock::given(method("GET"))
            .and(path("/attachment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "downloadUrl": "https://example.test/download",
                "embedUrl": "https://example.test/embed",
                "contentLength": 128,
                "status": {"upload_status": "done"}
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.attachments()
                .get(GetAttachmentRequest {
                    filename: "example.txt".to_string(),
                    content_type: "text/plain".to_string(),
                    org_id: "org-id".to_string(),
                    key: Some("attachment-key".to_string()),
                    url: None,
                })
                .await
                .expect("get")
                .content_length(),
            Some(128)
        );

        Mock::given(method("POST"))
            .and(path("/attachment/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success"
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.attachments()
                .put_status(PutAttachmentStatusRequest {
                    status: AttachmentStatus {
                        upload_status: UploadStatus::Done,
                        error_message: None,
                    },
                    org_id: "org-id".to_string(),
                    key: "attachment-key".to_string(),
                })
                .await
                .expect("put status")
                .status(),
            "success"
        );

        let requests = api_server.received_requests().await.expect("requests");
        let create = request_with_body(&requests, "POST", "/attachment");
        assert_eq!(auth_header(create), "Bearer token");
        assert_eq!(json_body(create)["filename"], "example.txt");

        let get = request(&requests, "GET", "/attachment");
        assert_query_value(get, "key", "attachment-key");
        assert_query_value(get, "org_id", "org-id");

        let status = request_with_body(&requests, "POST", "/attachment/status");
        assert_eq!(json_body(status)["status"]["upload_status"], "done");
        assert_eq!(json_body(status)["key"], "attachment-key");

        let app_requests = app_server.received_requests().await.expect("app requests");
        assert!(app_requests.is_empty());
    }

    #[tokio::test]
    async fn attachment_non_success_status_maps_to_error() {
        let server = MockServer::start().await;
        let api = api_client_with_urls(server.uri(), server.uri(), "");

        Mock::given(method("POST"))
            .and(path("/attachment/status"))
            .respond_with(ResponseTemplate::new(409).set_body_string("not ready"))
            .mount(&server)
            .await;

        let err = api
            .attachments()
            .put_status(PutAttachmentStatusRequest {
                status: AttachmentStatus {
                    upload_status: UploadStatus::Error,
                    error_message: Some("failed".to_string()),
                },
                org_id: "org-id".to_string(),
                key: "attachment-key".to_string(),
            })
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            crate::BraintrustError::Api {
                status: 409,
                ref message
            } if message == "not ready"
        ));
    }
}
