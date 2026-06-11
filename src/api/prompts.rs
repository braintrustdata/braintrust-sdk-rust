//! Prompt resource endpoints.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::client::{path_segment, ApiClient};
pub use crate::api::function_types::{
    FunctionType, Prompt, PromptData, PromptOptions, PromptParser, TemplateFormat,
};

/// Client for prompt API operations.
#[derive(Clone, Debug)]
pub struct PromptsClient {
    api: ApiClient,
}

impl PromptsClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    /// Lists prompts.
    pub async fn list(&self, request: ListPromptsRequest) -> crate::Result<ListPromptsResponse> {
        self.api
            .get_json_with_query("v1/prompt", &request, "prompt list response")
            .await
    }

    /// Fetches one prompt by id.
    pub async fn get(&self, prompt_id: &str) -> crate::Result<Prompt> {
        self.api
            .get_json(
                &format!("v1/prompt/{}", path_segment(prompt_id)),
                "prompt response",
            )
            .await
    }

    /// Creates a prompt.
    pub async fn create(&self, request: PromptRequest) -> crate::Result<Prompt> {
        self.api
            .post_json("v1/prompt", &request, "prompt create response")
            .await
    }

    /// Deletes one prompt by id.
    pub async fn delete(&self, prompt_id: &str) -> crate::Result<DeletePromptResponse> {
        self.api
            .delete_json(
                &format!("v1/prompt/{}", path_segment(prompt_id)),
                "prompt delete response",
            )
            .await
    }
}

/// Query parameters for listing prompts.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ListPromptsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
}

/// Request body for creating prompts.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct PromptRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_data: Option<PromptData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_type: Option<FunctionType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    // TODO: Harden this Value-backed metadata map if backend metadata becomes schema-constrained.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    // TODO: Harden this Value-backed extension map to concrete prompt request fields.
    #[serde(flatten, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// Response returned by [`PromptsClient::list()`].
#[derive(Debug, Clone, Deserialize)]
pub struct ListPromptsResponse {
    #[serde(default)]
    objects: Vec<Prompt>,
}

impl ListPromptsResponse {
    api_getters! {
        slice objects: Prompt;
    }
}

/// Response returned by [`PromptsClient::delete()`].
#[derive(Debug, Clone, Deserialize)]
pub struct DeletePromptResponse {
    #[serde(default)]
    id: Option<String>,
    // TODO: Harden this Value-backed extension map to the concrete prompt delete response schema.
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl DeletePromptResponse {
    api_getters! {
        option_str id;
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
        api_client_with_urls, assert_query_value, auth_header, json_body, query_values, request,
        request_with_body,
    };

    #[tokio::test]
    async fn prompt_routes_use_api_url_methods_auth_org_and_json() {
        let api_server = MockServer::start().await;
        let app_server = MockServer::start().await;
        let api = api_client_with_urls(api_server.uri(), app_server.uri(), "Org");

        Mock::given(method("GET"))
            .and(path("/v1/prompt"))
            .and(header("authorization", "Bearer token"))
            .and(header("x-bt-org-name", "Org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "objects": [{"id": "prompt-id", "name": "Prompt"}]
            })))
            .mount(&api_server)
            .await;

        let list = api
            .prompts()
            .list(ListPromptsRequest {
                project_id: Some("project-id".to_string()),
                project_name: Some("Project".to_string()),
                name: Some("Prompt".to_string()),
                ids: vec!["prompt-id".to_string()],
            })
            .await
            .expect("list");
        assert_eq!(list.objects()[0].id(), "prompt-id");

        Mock::given(method("GET"))
            .and(path("/v1/prompt/prompt-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "prompt-id",
                "name": "Prompt"
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.prompts().get("prompt-id").await.expect("get").name(),
            Some("Prompt")
        );

        Mock::given(method("POST"))
            .and(path("/v1/prompt"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "created-prompt-id",
                "name": "Prompt"
            })))
            .mount(&api_server)
            .await;
        api.prompts()
            .create(PromptRequest {
                project_id: Some("project-id".to_string()),
                name: Some("Prompt".to_string()),
                ..Default::default()
            })
            .await
            .expect("create");

        Mock::given(method("DELETE"))
            .and(path("/v1/prompt/prompt-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "prompt-id"
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.prompts()
                .delete("prompt-id")
                .await
                .expect("delete")
                .id(),
            Some("prompt-id")
        );

        let requests = api_server.received_requests().await.expect("requests");
        let list_request = request(&requests, "GET", "/v1/prompt");
        assert_query_value(list_request, "project_id", "project-id");
        assert_query_value(list_request, "project_name", "Project");
        assert_query_value(list_request, "name", "Prompt");
        assert_eq!(query_values(list_request, "ids"), vec!["prompt-id"]);

        let create = request_with_body(&requests, "POST", "/v1/prompt");
        assert_eq!(auth_header(create), "Bearer token");
        assert_eq!(json_body(create)["project_id"], "project-id");

        let app_requests = app_server.received_requests().await.expect("app requests");
        assert!(app_requests.is_empty());
    }

    #[tokio::test]
    async fn prompt_non_success_status_maps_to_error() {
        let server = MockServer::start().await;
        let api = api_client_with_urls(server.uri(), server.uri(), "");

        Mock::given(method("GET"))
            .and(path("/v1/prompt"))
            .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
            .mount(&server)
            .await;

        let err = api
            .prompts()
            .list(ListPromptsRequest::default())
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
