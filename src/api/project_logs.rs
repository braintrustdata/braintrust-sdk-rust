//! Project log event endpoints.
//!
//! This module exposes the low-level `/v1/project_logs` fetch API. It is
//! useful for validating that high-level logging and span ingestion has been
//! persisted in the backend.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::client::ApiClient;
use crate::api::FetchEventsRequest;

/// Client for project-log API operations.
///
/// Create this through [`crate::ApiClient::project_logs()`].
#[derive(Clone, Debug)]
pub struct ProjectLogsClient {
    api: ApiClient,
}

impl ProjectLogsClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    api_endpoint_methods! {
        post_child(fetch, project_id, FetchEventsRequest, FetchProjectLogsEventsResponse, "v1/project_logs", "fetch", "project logs fetch response");
    }
}

/// Response returned by [`ProjectLogsClient::fetch()`].
///
/// Contains a page of project-log events and an optional cursor for the next
/// page.
#[derive(Debug, Clone, Deserialize)]
pub struct FetchProjectLogsEventsResponse {
    events: Vec<ProjectLogsEvent>,
    #[serde(default)]
    cursor: Option<String>,
}

impl FetchProjectLogsEventsResponse {
    api_getters! {
        slice events: ProjectLogsEvent;
        option_str cursor;
    }
}

/// Event row returned by [`ProjectLogsClient::fetch()`].
///
/// Project-log events expose logged input, output, scores, metrics, and span
/// metadata. Unknown backend fields are preserved in
/// [`ProjectLogsEvent::extra()`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectLogsEvent {
    id: String,
    #[serde(rename = "_xact_id")]
    xact_id: String,
    created: String,
    org_id: String,
    project_id: String,
    log_id: String,
    span_id: String,
    root_span_id: String,
    #[serde(
        default,
        rename = "_pagination_key",
        skip_serializing_if = "Option::is_none"
    )]
    pagination_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scores: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metrics: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    span_parents: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    span_attributes: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    is_root: Option<bool>,
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl ProjectLogsEvent {
    api_getters! {
        str id;
        str xact_id;
        str created;
        str org_id;
        str project_id;
        str log_id;
        str span_id;
        str root_span_id;
        option_str pagination_key;
        option_ref input: Value;
        option_ref output: Value;
        option_ref expected: Value;
        option_ref error: Value;
        option_map scores;
        option_map metadata;
        option_map metrics;
        option_slice tags: String;
        option_map context;
        option_slice span_parents: String;
        option_ref span_attributes: Value;
        option_ref is_root: bool;
        map extra;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::api::test_support::{api_client, auth_header, json_body, request_with_body};

    #[tokio::test]
    async fn fetch_sends_expected_request() {
        let server = MockServer::start().await;
        let api = api_client(server.uri());

        Mock::given(method("POST"))
            .and(path("/v1/project_logs/project-id/fetch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "events": [{
                    "id": "event-id",
                    "_xact_id": "xact-id",
                    "created": "2026-01-01T00:00:00Z",
                    "org_id": "org-id",
                    "project_id": "project-id",
                    "log_id": "g",
                    "input": {"prompt": "hello"},
                    "output": {"answer": "world"},
                    "span_id": "span-id",
                    "root_span_id": "root-span-id"
                }],
                "cursor": null
            })))
            .mount(&server)
            .await;

        let fetched = api
            .project_logs()
            .fetch(
                "project-id",
                FetchEventsRequest {
                    limit: Some(1),
                    version: Some("version-id".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("fetch project logs");
        assert_eq!(fetched.events()[0].log_id(), "g");

        let requests = server.received_requests().await.expect("requests");
        let fetch = request_with_body(&requests, "POST", "/v1/project_logs/project-id/fetch");
        assert_eq!(auth_header(fetch), "Bearer token");
        assert_eq!(json_body(fetch)["limit"], 1);
        assert_eq!(json_body(fetch)["version"], "version-id");
    }
}
