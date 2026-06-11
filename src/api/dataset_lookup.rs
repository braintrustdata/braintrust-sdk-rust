//! App-backed dataset lookup endpoint.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::client::{ApiBase, ApiClient};

/// Client for app-backed dataset lookup operations.
#[derive(Clone, Debug)]
pub struct DatasetLookupClient {
    api: ApiClient,
}

impl DatasetLookupClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    /// Fetches dataset lookup data through `/api/dataset/get` on the app URL.
    pub async fn get(&self, request: DatasetLookupRequest) -> crate::Result<DatasetLookupResponse> {
        self.api
            .post_json_to(
                ApiBase::App,
                "api/dataset/get",
                &request,
                "dataset lookup response",
            )
            .await
    }
}

/// Request body for `/api/dataset/get`.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct DatasetLookupRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starting_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ending_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// Response returned by `/api/dataset/get`.
#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub struct DatasetLookupResponse {
    objects: Vec<DatasetLookupResult>,
}

impl DatasetLookupResponse {
    api_getters! {
        slice objects: DatasetLookupResult;
    }
}

/// Dataset lookup result returned by the app API.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct DatasetLookupResult {
    id: String,
    project_id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deleted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<String>>,
    // TODO: Harden this Value-backed metadata map if dataset metadata becomes schema-constrained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata: Option<Map<String, Value>>,
    // TODO: Harden this Value-backed extension map to concrete dataset lookup fields as they stabilize.
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl DatasetLookupResult {
    api_getters! {
        str id;
        str project_id;
        str name;
        option_str description;
        option_str created;
        option_str deleted_at;
        option_str user_id;
        option_slice tags: String;
        option_map metadata;
        map extra;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::api::test_support::{
        api_client_with_urls, auth_header, json_body, request_with_body,
    };

    #[tokio::test]
    async fn dataset_lookup_uses_app_url_auth_org_and_json() {
        let api_server = MockServer::start().await;
        let app_server = MockServer::start().await;
        let api = api_client_with_urls(api_server.uri(), app_server.uri(), "Org");

        Mock::given(method("POST"))
            .and(path("/api/dataset/get"))
            .and(header("authorization", "Bearer token"))
            .and(header("x-bt-org-name", "Org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id": "dataset-id",
                "project_id": "project-id",
                "name": "Dataset"
            }])))
            .mount(&app_server)
            .await;

        assert_eq!(
            api.dataset_lookup()
                .get(DatasetLookupRequest {
                    id: Some("dataset-id".to_string()),
                    project_name: Some("Project".to_string()),
                    name: Some("Dataset".to_string()),
                    ..Default::default()
                })
                .await
                .expect("get")
                .objects()[0]
                .id(),
            "dataset-id"
        );

        let requests = app_server.received_requests().await.expect("requests");
        let request = request_with_body(&requests, "POST", "/api/dataset/get");
        assert_eq!(auth_header(request), "Bearer token");
        assert_eq!(json_body(request)["id"], "dataset-id");
        assert_eq!(json_body(request)["project_name"], "Project");
        assert_eq!(json_body(request)["name"], "Dataset");

        let api_requests = api_server.received_requests().await.expect("api requests");
        assert!(api_requests.is_empty());
    }

    #[tokio::test]
    async fn dataset_lookup_non_success_status_maps_to_error() {
        let server = MockServer::start().await;
        let api = api_client_with_urls(server.uri(), server.uri(), "");

        Mock::given(method("POST"))
            .and(path("/api/dataset/get"))
            .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
            .mount(&server)
            .await;

        let err = api
            .dataset_lookup()
            .get(DatasetLookupRequest::default())
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            crate::BraintrustError::Api {
                status: 404,
                ref message
            } if message == "missing"
        ));
    }
}
