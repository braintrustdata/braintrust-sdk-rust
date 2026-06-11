//! API key resource endpoints.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::client::{path_segment, ApiClient};

/// Client for API key operations.
#[derive(Clone, Debug)]
pub struct ApiKeysClient {
    api: ApiClient,
}

impl ApiKeysClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    /// Lists API keys.
    pub async fn list(&self, request: ListApiKeysRequest) -> crate::Result<ListApiKeysResponse> {
        self.api
            .get_json_with_query("v1/api_key", &request, "API key list response")
            .await
    }

    /// Creates an API key.
    pub async fn create(&self, request: CreateApiKeyRequest) -> crate::Result<CreateApiKeyOutput> {
        self.api
            .post_json("v1/api_key", &request, "API key create response")
            .await
    }

    /// Deletes an API key.
    pub async fn delete(&self, api_key_id: &str) -> crate::Result<ApiKey> {
        self.api
            .delete_json(
                &format!("v1/api_key/{}", path_segment(api_key_id)),
                "API key delete response",
            )
            .await
    }
}

/// Query parameters for listing API keys.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ListApiKeysRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starting_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ending_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// Request body for creating API keys.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct CreateApiKeyRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
}

/// Response returned by [`ApiKeysClient::list()`].
#[derive(Debug, Clone, Deserialize)]
pub struct ListApiKeysResponse {
    #[serde(default)]
    objects: Vec<ApiKey>,
}

impl ListApiKeysResponse {
    api_getters! {
        slice objects: ApiKey;
    }
}

/// API key object returned by the API.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ApiKey {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    name: String,
    preview_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_given_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_family_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    org_id: Option<String>,
    // TODO: Harden this Value-backed extension map to concrete API key fields as they stabilize.
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl ApiKey {
    api_getters! {
        str id;
        option_str created;
        str name;
        str preview_name;
        option_str user_id;
        option_str user_email;
        option_str user_given_name;
        option_str user_family_name;
        option_str org_id;
        map extra;
    }
}

/// API key creation response, including the one-time raw key.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CreateApiKeyOutput {
    #[serde(flatten)]
    api_key: ApiKey,
    key: String,
}

impl CreateApiKeyOutput {
    api_getters! {
        ref api_key: ApiKey;
        str key;
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
    async fn api_key_routes_use_api_url_methods_auth_org_and_json() {
        let api_server = MockServer::start().await;
        let app_server = MockServer::start().await;
        let api = api_client_with_urls(api_server.uri(), app_server.uri(), "Org");

        Mock::given(method("GET"))
            .and(path("/v1/api_key"))
            .and(header("authorization", "Bearer token"))
            .and(header("x-bt-org-name", "Org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "objects": [{
                    "id": "api-key-id",
                    "name": "Key",
                    "preview_name": "sk-..."
                }]
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.api_keys()
                .list(ListApiKeysRequest {
                    limit: Some(10),
                    starting_after: None,
                    ending_before: None,
                    name: Some("Key".to_string()),
                    org_name: Some("Org".to_string()),
                    org_id: Some("org-id".to_string()),
                    id: Some("api-key-id".to_string()),
                })
                .await
                .expect("list")
                .objects()[0]
                .id(),
            "api-key-id"
        );

        Mock::given(method("POST"))
            .and(path("/v1/api_key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "created-api-key-id",
                "name": "Key",
                "preview_name": "sk-...",
                "key": "sk-test"
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.api_keys()
                .create(CreateApiKeyRequest {
                    name: "Key".to_string(),
                    org_name: Some("Org".to_string()),
                })
                .await
                .expect("create")
                .key(),
            "sk-test"
        );

        Mock::given(method("DELETE"))
            .and(path("/v1/api_key/api-key-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "api-key-id",
                "name": "Key",
                "preview_name": "sk-..."
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.api_keys()
                .delete("api-key-id")
                .await
                .expect("delete")
                .id(),
            "api-key-id"
        );

        let requests = api_server.received_requests().await.expect("requests");
        let list = request(&requests, "GET", "/v1/api_key");
        assert_query_value(list, "limit", "10");
        assert_query_value(list, "name", "Key");
        assert_query_value(list, "org_name", "Org");
        assert_query_value(list, "org_id", "org-id");
        assert_query_value(list, "id", "api-key-id");

        let create = request_with_body(&requests, "POST", "/v1/api_key");
        assert_eq!(auth_header(create), "Bearer token");
        assert_eq!(json_body(create)["name"], "Key");
        assert_eq!(json_body(create)["org_name"], "Org");

        let app_requests = app_server.received_requests().await.expect("app requests");
        assert!(app_requests.is_empty());
    }

    #[tokio::test]
    async fn api_key_non_success_status_maps_to_error() {
        let server = MockServer::start().await;
        let api = api_client_with_urls(server.uri(), server.uri(), "");

        Mock::given(method("GET"))
            .and(path("/v1/api_key"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let err = api
            .api_keys()
            .list(ListApiKeysRequest::default())
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            crate::BraintrustError::Api {
                status: 401,
                ref message
            } if message == "unauthorized"
        ));
    }
}
