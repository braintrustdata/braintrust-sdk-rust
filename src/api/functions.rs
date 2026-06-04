//! Function resource endpoints.
//!
//! Functions have broad backend-managed schemas, especially for invocation
//! payloads and code bundles. This module keeps the HTTP routes typed while
//! preserving dynamic fields for callers that mirror `bt` internals.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

use crate::api::client::{path_segment, ApiClient};
pub use crate::api::function_types::{
    CacheControl, CacheControlType, ChatCompletionAssistantContent, ChatCompletionAssistantMessage,
    ChatCompletionContent, ChatCompletionContentPart, ChatCompletionContentPartFile,
    ChatCompletionContentPartFileFile, ChatCompletionContentPartImage,
    ChatCompletionContentPartText, ChatCompletionDeveloperMessage, ChatCompletionFallbackMessage,
    ChatCompletionFunctionCall, ChatCompletionFunctionMessage, ChatCompletionImageDetail,
    ChatCompletionImageUrl, ChatCompletionMessage, ChatCompletionMessageReasoning,
    ChatCompletionMessageToolCall, ChatCompletionNullableString, ChatCompletionSystemMessage,
    ChatCompletionTextContent, ChatCompletionToolMessage, ChatCompletionUserMessage, Function,
    FunctionData, FunctionSchema, FunctionType, InvokeParent, McpAuth, MessageRole, PromptData,
    SavedFunctionId, StreamingMode,
};

/// Client for function API operations.
#[derive(Clone, Debug)]
pub struct FunctionsClient {
    api: ApiClient,
}

impl FunctionsClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    /// Lists functions.
    pub async fn list(
        &self,
        request: ListFunctionsRequest,
    ) -> crate::Result<ListFunctionsResponse> {
        self.api
            .get_json_with_query("v1/function", &request, "function list response")
            .await
    }

    /// Fetches one function by id.
    pub async fn get(&self, function_id: &str) -> crate::Result<Function> {
        self.api
            .get_json(
                &format!("v1/function/{}", path_segment(function_id)),
                "function response",
            )
            .await
    }

    /// Creates a function.
    pub async fn create(&self, request: FunctionRequest) -> crate::Result<Function> {
        self.api
            .post_json("v1/function", &request, "function create response")
            .await
    }

    /// Patches one function by id.
    pub async fn patch(
        &self,
        function_id: &str,
        request: FunctionRequest,
    ) -> crate::Result<Function> {
        self.api
            .patch_json(
                &format!("v1/function/{}", path_segment(function_id)),
                &request,
                "function patch response",
            )
            .await
    }

    /// Deletes one function by id.
    pub async fn delete(&self, function_id: &str) -> crate::Result<DeleteFunctionResponse> {
        self.api
            .delete_json(
                &format!("v1/function/{}", path_segment(function_id)),
                "function delete response",
            )
            .await
    }

    /// Fetches or updates function code through `/function/code`.
    pub async fn code(&self, request: FunctionRequest) -> crate::Result<FunctionCodeResponse> {
        self.api
            .post_json("function/code", &request, "function code response")
            .await
    }

    /// Invokes a function through `/function/invoke`.
    pub async fn invoke(
        &self,
        request: InvokeFunctionRequest,
    ) -> crate::Result<InvokeFunctionResponse> {
        self.api
            .post_json("function/invoke", &request, "function invoke response")
            .await
    }

    /// Inserts multiple functions through `/insert-functions`.
    pub async fn insert(
        &self,
        request: InsertFunctionsRequest,
    ) -> crate::Result<InsertFunctionsResponse> {
        self.api
            .post_json("insert-functions", &request, "insert functions response")
            .await
    }
}

/// Query parameters for listing functions.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ListFunctionsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
}

/// Request body for function create, patch, and code routes.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct FunctionRequest {
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
    pub function_data: Option<FunctionData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_type: Option<FunctionType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_schema: Option<FunctionSchema>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    // TODO: Harden this Value-backed metadata map if backend metadata becomes schema-constrained.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub if_exists: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environments: Option<Vec<FunctionEnvironment>>,
    // TODO: Harden this Value-backed extension map to concrete function request fields.
    #[serde(flatten, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// Request body for invoking a function.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct InvokeFunctionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_type: Option<FunctionType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_session_function_id: Option<String>,
    // TODO: Harden this Value-backed invocation input if function schemas become statically known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    // TODO: Harden this Value-backed invocation expected value if function schemas become statically known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
    // TODO: Harden this Value-backed metadata map if backend metadata becomes schema-constrained.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<ChatCompletionMessage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<InvokeParent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<StreamingMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_auth: Option<BTreeMap<String, McpAuth>>,
    // TODO: Harden this Value-backed overrides map to concrete invocation override fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<Map<String, Value>>,
    // TODO: Harden this Value-backed extension map to concrete invocation request variants.
    #[serde(flatten, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// Request body for inserting functions.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct InsertFunctionsRequest {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<FunctionRequest>,
}

/// Environment selector used by `/insert-functions`.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct FunctionEnvironment {
    pub slug: String,
}

/// Response returned by [`FunctionsClient::list()`].
#[derive(Debug, Clone, Deserialize)]
pub struct ListFunctionsResponse {
    #[serde(default)]
    objects: Vec<Function>,
    #[serde(default)]
    next_cursor: Option<String>,
    #[serde(default)]
    snapshot: Option<String>,
}

impl ListFunctionsResponse {
    api_getters! {
        slice objects: Function;
        option_str next_cursor;
        option_str snapshot;
    }
}

/// Response returned by [`FunctionsClient::delete()`].
#[derive(Debug, Clone, Deserialize)]
pub struct DeleteFunctionResponse {
    #[serde(default)]
    id: Option<String>,
    // TODO: Harden this Value-backed extension map to the concrete function delete response schema.
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl DeleteFunctionResponse {
    api_getters! {
        option_str id;
        map extra;
    }
}

/// Response returned by [`FunctionsClient::code()`].
#[derive(Debug, Clone, Deserialize)]
pub struct FunctionCodeResponse {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    code_hash: Option<String>,
    // TODO: Harden this Value-backed extension map to the concrete function/code response schema.
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl FunctionCodeResponse {
    api_getters! {
        option_str code;
        option_str code_hash;
        map extra;
    }
}

/// Response returned by [`FunctionsClient::invoke()`].
#[derive(Debug, Clone, Deserialize)]
pub struct InvokeFunctionResponse {
    // TODO: Harden this Value-backed invocation output when function schemas become statically known.
    #[serde(default)]
    output: Option<Value>,
    // TODO: Harden this Value-backed invocation error when backend error payload variants are fixed.
    #[serde(default)]
    error: Option<Value>,
    // TODO: Harden this Value-backed metadata map if backend metadata becomes schema-constrained.
    #[serde(default)]
    metadata: Option<Map<String, Value>>,
    // TODO: Harden this Value-backed extension map to concrete invocation response fields.
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl InvokeFunctionResponse {
    api_getters! {
        option_ref output: Value;
        option_ref error: Value;
        option_map metadata;
        map extra;
    }
}

/// Response returned by [`FunctionsClient::insert()`].
#[derive(Debug, Clone, Deserialize)]
pub struct InsertFunctionsResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    xact_id: Option<String>,
    #[serde(default)]
    functions: Vec<Function>,
    // TODO: Harden this Value-backed extension map to concrete insert-functions response fields.
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl InsertFunctionsResponse {
    api_getters! {
        option_str status;
        option_str xact_id;
        slice functions: Function;
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

    fn request_body() -> FunctionRequest {
        FunctionRequest {
            project_id: Some("project-id".to_string()),
            slug: Some("slug".to_string()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn function_routes_use_api_url_methods_auth_org_and_json() {
        let api_server = MockServer::start().await;
        let app_server = MockServer::start().await;
        let api = api_client_with_urls(api_server.uri(), app_server.uri(), "Org");

        Mock::given(method("GET"))
            .and(path("/v1/function"))
            .and(header("authorization", "Bearer token"))
            .and(header("x-bt-org-name", "Org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "objects": [{"id": "function-id", "slug": "slug"}],
                "next_cursor": "next",
                "snapshot": "snapshot-id"
            })))
            .mount(&api_server)
            .await;

        let list = api
            .functions()
            .list(ListFunctionsRequest {
                project_id: Some("project-id".to_string()),
                project_name: Some("Project".to_string()),
                slug: Some("slug".to_string()),
                ids: vec!["function-id".to_string()],
                version: Some("1".to_string()),
                cursor: Some("cursor".to_string()),
                snapshot: Some("snapshot-id".to_string()),
            })
            .await
            .expect("list");
        assert_eq!(list.objects()[0].id(), "function-id");
        assert_eq!(list.next_cursor(), Some("next"));
        assert_eq!(list.snapshot(), Some("snapshot-id"));

        Mock::given(method("GET"))
            .and(path("/v1/function/function-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "function-id",
                "slug": "slug"
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.functions()
                .get("function-id")
                .await
                .expect("get")
                .slug(),
            Some("slug")
        );

        Mock::given(method("POST"))
            .and(path("/v1/function"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "created-function-id",
                "slug": "slug"
            })))
            .mount(&api_server)
            .await;
        api.functions()
            .create(request_body())
            .await
            .expect("create");

        Mock::given(method("PATCH"))
            .and(path("/v1/function/function-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "function-id",
                "slug": "patched"
            })))
            .mount(&api_server)
            .await;
        api.functions()
            .patch("function-id", request_body())
            .await
            .expect("patch");

        Mock::given(method("DELETE"))
            .and(path("/v1/function/function-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "function-id"
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.functions()
                .delete("function-id")
                .await
                .expect("delete")
                .id(),
            Some("function-id")
        );

        Mock::given(method("POST"))
            .and(path("/function/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": "return input"
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.functions()
                .code(request_body())
                .await
                .expect("code")
                .code(),
            Some("return input")
        );

        Mock::given(method("POST"))
            .and(path("/function/invoke"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "output": 42
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.functions()
                .invoke(InvokeFunctionRequest {
                    function_id: Some("function-id".to_string()),
                    input: Some(json!({"x": 1})),
                    ..Default::default()
                })
                .await
                .expect("invoke")
                .output(),
            Some(&json!(42))
        );

        Mock::given(method("POST"))
            .and(path("/insert-functions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "xact_id": "xact-id",
                "functions": [{"id": "inserted-function-id"}]
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.functions()
                .insert(InsertFunctionsRequest {
                    functions: vec![request_body()]
                })
                .await
                .expect("insert")
                .functions()[0]
                .id(),
            "inserted-function-id"
        );

        let requests = api_server.received_requests().await.expect("requests");
        let list_request = request(&requests, "GET", "/v1/function");
        assert_query_value(list_request, "project_id", "project-id");
        assert_query_value(list_request, "project_name", "Project");
        assert_query_value(list_request, "slug", "slug");
        assert_eq!(query_values(list_request, "ids"), vec!["function-id"]);
        assert_query_value(list_request, "version", "1");
        assert_query_value(list_request, "cursor", "cursor");
        assert_query_value(list_request, "snapshot", "snapshot-id");

        let create = request_with_body(&requests, "POST", "/v1/function");
        assert_eq!(auth_header(create), "Bearer token");
        assert_eq!(json_body(create)["project_id"], "project-id");

        let app_requests = app_server.received_requests().await.expect("app requests");
        assert!(app_requests.is_empty());
    }

    #[tokio::test]
    async fn function_non_success_status_maps_to_error() {
        let server = MockServer::start().await;
        let api = api_client_with_urls(server.uri(), server.uri(), "");

        Mock::given(method("GET"))
            .and(path("/v1/function"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let err = api
            .functions()
            .list(ListFunctionsRequest::default())
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
