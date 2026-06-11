//! AI secret lookup endpoints.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::client::{ApiBase, ApiClient};

/// Client for AI secret API operations.
#[derive(Clone, Debug)]
pub struct AiSecretsClient {
    api: ApiClient,
}

impl AiSecretsClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    /// Fetches AI secret data through `/api/ai_secret/get` on the app URL.
    pub async fn get(&self, request: AiSecretGetRequest) -> crate::Result<AiSecretGetResponse> {
        self.api
            .post_json_to(
                ApiBase::App,
                "api/ai_secret/get",
                &request,
                "AI secret get response",
            )
            .await
    }
}

/// Request body for `/api/ai_secret/get`.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct AiSecretGetRequest {
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
    #[serde(rename = "type", skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
}

/// Response returned by `/api/ai_secret/get`.
#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub struct AiSecretGetResponse {
    objects: Vec<AiSecret>,
}

impl AiSecretGetResponse {
    api_getters! {
        slice objects: AiSecret;
    }
}

/// AI secret object returned by the API.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct AiSecret {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret_updated_at: Option<String>,
    org_id: String,
    name: String,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    secret_type: Option<String>,
    // TODO: Harden this Value-backed metadata map if AI secret metadata becomes schema-constrained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret_updated_by_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preview_secret: Option<String>,
    // TODO: Harden this Value-backed extension map to concrete AI secret fields as they stabilize.
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl AiSecret {
    api_getters! {
        str id;
        option_str created;
        option_str updated_at;
        option_str secret_updated_at;
        str org_id;
        str name;
        option_str secret_type;
        option_map metadata;
        option_str secret_updated_by_user_id;
        option_str preview_secret;
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
    async fn ai_secret_get_uses_app_url_auth_org_and_json() {
        let api_server = MockServer::start().await;
        let app_server = MockServer::start().await;
        let api = api_client_with_urls(api_server.uri(), app_server.uri(), "Org");

        Mock::given(method("POST"))
            .and(path("/api/ai_secret/get"))
            .and(header("authorization", "Bearer token"))
            .and(header("x-bt-org-name", "Org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id": "secret-id",
                "org_id": "org-id",
                "name": "OPENAI_API_KEY",
                "type": "llm",
                "preview_secret": "sk-..."
            }])))
            .mount(&app_server)
            .await;

        assert_eq!(
            api.ai_secrets()
                .get(AiSecretGetRequest {
                    name: Some("OPENAI_API_KEY".to_string()),
                    org_name: Some("Org".to_string()),
                    types: vec!["llm".to_string()],
                    ..Default::default()
                })
                .await
                .expect("get")
                .objects()[0]
                .name(),
            "OPENAI_API_KEY"
        );

        let requests = app_server.received_requests().await.expect("requests");
        let request = request_with_body(&requests, "POST", "/api/ai_secret/get");
        assert_eq!(auth_header(request), "Bearer token");
        assert_eq!(json_body(request)["name"], "OPENAI_API_KEY");
        assert_eq!(json_body(request)["org_name"], "Org");
        assert_eq!(json_body(request)["type"], json!(["llm"]));

        let api_requests = api_server.received_requests().await.expect("api requests");
        assert!(api_requests.is_empty());
    }

    #[tokio::test]
    async fn ai_secret_non_success_status_maps_to_error() {
        let server = MockServer::start().await;
        let api = api_client_with_urls(server.uri(), server.uri(), "");

        Mock::given(method("POST"))
            .and(path("/api/ai_secret/get"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = api
            .ai_secrets()
            .get(AiSecretGetRequest::default())
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            crate::BraintrustError::Api {
                status: 403,
                ref message
            } if message == "forbidden"
        ));
    }
}
