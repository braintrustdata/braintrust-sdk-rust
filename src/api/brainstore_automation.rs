//! Brainstore automation cursor endpoints.

use serde::{Deserialize, Serialize};

use crate::api::client::ApiClient;

/// Client for Brainstore automation API operations.
#[derive(Clone, Debug)]
pub struct BrainstoreAutomationClient {
    api: ApiClient,
}

impl BrainstoreAutomationClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    /// Calls `/brainstore/automation/get-cursors`.
    pub async fn get_cursors(
        &self,
        request: GetAutomationCursorsRequest,
    ) -> crate::Result<AutomationCursorStatus> {
        self.api
            .post_json(
                "brainstore/automation/get-cursors",
                &request,
                "Brainstore automation get cursors response",
            )
            .await
    }

    /// Calls `/brainstore/automation/get-object-cursors`.
    pub async fn get_object_cursors(
        &self,
        request: GetObjectAutomationCursorsRequest,
    ) -> crate::Result<ObjectAutomationCursorStatus> {
        self.api
            .post_json(
                "brainstore/automation/get-object-cursors",
                &request,
                "Brainstore automation get object cursors response",
            )
            .await
    }

    /// Calls `/brainstore/automation/reset-cursors`.
    pub async fn reset_cursors(
        &self,
        request: ResetAutomationCursorsRequest,
    ) -> crate::Result<AutomationCursorMutationResponse> {
        self.api
            .post_json(
                "brainstore/automation/reset-cursors",
                &request,
                "Brainstore automation reset cursors response",
            )
            .await
    }

    /// Calls `/brainstore/automation/upsert-object-cursor`.
    pub async fn upsert_object_cursor(
        &self,
        request: UpsertObjectAutomationCursorRequest,
    ) -> crate::Result<AutomationCursorMutationResponse> {
        self.api
            .post_json(
                "brainstore/automation/upsert-object-cursor",
                &request,
                "Brainstore automation upsert object cursor response",
            )
            .await
    }
}

/// Request body for `/brainstore/automation/get-cursors`.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct GetAutomationCursorsRequest {
    pub automation_id: String,
    pub project_id: String,
}

/// Request body for `/brainstore/automation/get-object-cursors`.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct GetObjectAutomationCursorsRequest {
    pub automation_id: String,
    pub project_id: String,
}

/// Request body for `/brainstore/automation/reset-cursors`.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ResetAutomationCursorsRequest {
    pub automation_id: String,
    pub object_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_xact_id: Option<AutomationStartXactId>,
}

/// Request body for `/brainstore/automation/upsert-object-cursor`.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct UpsertObjectAutomationCursorRequest {
    pub automation_id: String,
    pub object_id: String,
}

/// Starting transaction id accepted by Brainstore cursor reset.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
#[non_exhaustive]
pub enum AutomationStartXactId {
    String(String),
    Number(i64),
}

/// Response returned by cursor reset and upsert endpoints.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct AutomationCursorMutationResponse {
    success: bool,
    automation_id: String,
}

impl AutomationCursorMutationResponse {
    api_getters! {
        bool success;
        str automation_id;
    }
}

/// Aggregated cursor execution statistics.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CursorExecutionStats {
    #[serde(default)]
    rows_processed: f64,
    #[serde(default)]
    bytes_processed: f64,
    #[serde(default)]
    duration_ms: f64,
}

impl CursorExecutionStats {
    api_getters! {
        f64 rows_processed;
        f64 bytes_processed;
        f64 duration_ms;
    }
}

/// Cursor stats for the latest execution and all executions.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CursorStats {
    last_execution: CursorExecutionStats,
    all_executions: CursorExecutionStats,
}

impl CursorStats {
    api_getters! {
        ref last_execution: CursorExecutionStats;
        ref all_executions: CursorExecutionStats;
    }
}

/// Automation cursor status returned by `/get-cursors`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct AutomationCursorStatus {
    total_segments: f64,
    pending_segments: f64,
    error_segments: f64,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    last_error_at: Option<String>,
    #[serde(default)]
    pending_min_compacted_xact_id: Option<f64>,
    #[serde(default)]
    pending_max_compacted_xact_id: Option<f64>,
    #[serde(default)]
    pending_min_executed_xact_id: Option<f64>,
    #[serde(default)]
    stats: Option<CursorStats>,
}

impl AutomationCursorStatus {
    api_getters! {
        f64 total_segments;
        f64 pending_segments;
        f64 error_segments;
        option_str last_error;
        option_str last_error_at;
        option_f64 pending_min_compacted_xact_id;
        option_f64 pending_max_compacted_xact_id;
        option_f64 pending_min_executed_xact_id;
        option_ref stats: CursorStats;
    }
}

/// Object automation cursor status returned by `/get-object-cursors`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ObjectAutomationCursorStatus {
    total_objects: f64,
    due_objects: f64,
    error_objects: f64,
    #[serde(default)]
    last_compacted_xact_id: Option<f64>,
    #[serde(default)]
    next_run_at: Option<String>,
    #[serde(default)]
    last_run_at: Option<String>,
    #[serde(default)]
    retry_after: Option<String>,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    last_error_at: Option<String>,
    #[serde(default)]
    topic_runtime: Option<TopicRuntimeStatus>,
}

impl ObjectAutomationCursorStatus {
    api_getters! {
        f64 total_objects;
        f64 due_objects;
        f64 error_objects;
        option_f64 last_compacted_xact_id;
        option_str next_run_at;
        option_str last_run_at;
        option_str retry_after;
        option_str last_error;
        option_str last_error_at;
        option_ref topic_runtime: TopicRuntimeStatus;
    }
}

/// Topic automation runtime state.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TopicRuntimeState {
    WaitingForFacets,
    RecomputingTopicMaps,
    PendingTopicClassificationBackfill,
    BackfillingTopicClassifications,
    Idle,
}

/// Topic automation runtime status.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct TopicRuntimeStatus {
    state: TopicRuntimeState,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    entered_at: Option<String>,
    #[serde(default)]
    selected_window_seconds: Option<f64>,
    #[serde(default)]
    generation_window_start_xact_id: Option<f64>,
    #[serde(default)]
    generation_window_end_xact_id: Option<f64>,
    #[serde(default)]
    topic_classification_backfill_start_xact_id: Option<f64>,
    #[serde(default)]
    active_topic_map_versions: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    window_candidates: Vec<TopicRuntimeWindowCandidate>,
}

impl TopicRuntimeStatus {
    api_getters! {
        ref state: TopicRuntimeState;
        option_str reason;
        option_str entered_at;
        option_f64 selected_window_seconds;
        option_f64 generation_window_start_xact_id;
        option_f64 generation_window_end_xact_id;
        option_f64 topic_classification_backfill_start_xact_id;
        slice window_candidates: TopicRuntimeWindowCandidate;
    }

    /// Returns active topic map versions keyed by function id.
    pub fn active_topic_map_versions(&self) -> &std::collections::BTreeMap<String, String> {
        &self.active_topic_map_versions
    }
}

/// Candidate runtime window for topic automation.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct TopicRuntimeWindowCandidate {
    window_seconds: f64,
    ready_topic_maps: f64,
    total_topic_maps: f64,
}

impl TopicRuntimeWindowCandidate {
    api_getters! {
        f64 window_seconds;
        f64 ready_topic_maps;
        f64 total_topic_maps;
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
    async fn brainstore_automation_routes_use_api_url_methods_auth_org_and_json() {
        let api_server = MockServer::start().await;
        let app_server = MockServer::start().await;
        let api = api_client_with_urls(api_server.uri(), app_server.uri(), "Org");

        Mock::given(method("POST"))
            .and(path("/brainstore/automation/get-cursors"))
            .and(header("authorization", "Bearer token"))
            .and(header("x-bt-org-name", "Org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_segments": 3,
                "pending_segments": 1,
                "error_segments": 0,
                "last_error": null,
                "last_error_at": null,
                "pending_min_compacted_xact_id": 10,
                "pending_max_compacted_xact_id": 20,
                "pending_min_executed_xact_id": 8,
                "stats": {
                    "last_execution": {
                        "rows_processed": 2,
                        "bytes_processed": 100,
                        "duration_ms": 30
                    },
                    "all_executions": {
                        "rows_processed": 10,
                        "bytes_processed": 500,
                        "duration_ms": 90
                    }
                }
            })))
            .mount(&api_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/brainstore/automation/get-object-cursors"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_objects": 4,
                "due_objects": 2,
                "error_objects": 0,
                "last_compacted_xact_id": 44,
                "next_run_at": null,
                "last_run_at": null,
                "retry_after": null,
                "last_error": null,
                "last_error_at": null,
                "topic_runtime": {
                    "state": "idle",
                    "reason": null,
                    "entered_at": null,
                    "selected_window_seconds": null,
                    "generation_window_start_xact_id": null,
                    "generation_window_end_xact_id": null,
                    "topic_classification_backfill_start_xact_id": null,
                    "active_topic_map_versions": {},
                    "window_candidates": []
                }
            })))
            .mount(&api_server)
            .await;
        for route in [
            "/brainstore/automation/reset-cursors",
            "/brainstore/automation/upsert-object-cursor",
        ] {
            Mock::given(method("POST"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "success": true,
                    "automation_id": "automation-id"
                })))
                .mount(&api_server)
                .await;
        }

        assert_eq!(
            api.brainstore_automation()
                .get_cursors(GetAutomationCursorsRequest {
                    automation_id: "automation-id".to_string(),
                    project_id: "project-id".to_string(),
                })
                .await
                .expect("get cursors")
                .total_segments(),
            3.0
        );
        assert_eq!(
            api.brainstore_automation()
                .get_object_cursors(GetObjectAutomationCursorsRequest {
                    automation_id: "automation-id".to_string(),
                    project_id: "project-id".to_string(),
                })
                .await
                .expect("get object cursors")
                .total_objects(),
            4.0
        );
        assert!(api
            .brainstore_automation()
            .reset_cursors(ResetAutomationCursorsRequest {
                automation_id: "automation-id".to_string(),
                object_id: "project_logs:project-id".to_string(),
                start_xact_id: Some(AutomationStartXactId::Number(42)),
            })
            .await
            .expect("reset cursors")
            .success());
        api.brainstore_automation()
            .upsert_object_cursor(UpsertObjectAutomationCursorRequest {
                automation_id: "automation-id".to_string(),
                object_id: "project_logs:project-id".to_string(),
            })
            .await
            .expect("upsert object cursor");

        let requests = api_server.received_requests().await.expect("requests");
        for route in [
            "/brainstore/automation/get-cursors",
            "/brainstore/automation/get-object-cursors",
            "/brainstore/automation/reset-cursors",
            "/brainstore/automation/upsert-object-cursor",
        ] {
            let request = request_with_body(&requests, "POST", route);
            assert_eq!(auth_header(request), "Bearer token");
        }
        let get = request_with_body(&requests, "POST", "/brainstore/automation/get-cursors");
        assert_eq!(json_body(get)["project_id"], "project-id");
        assert_eq!(json_body(get)["automation_id"], "automation-id");
        let reset = request_with_body(&requests, "POST", "/brainstore/automation/reset-cursors");
        assert_eq!(json_body(reset)["object_id"], "project_logs:project-id");
        assert_eq!(json_body(reset)["start_xact_id"], 42);

        let app_requests = app_server.received_requests().await.expect("app requests");
        assert!(app_requests.is_empty());
    }

    #[tokio::test]
    async fn brainstore_automation_non_success_status_maps_to_error() {
        let server = MockServer::start().await;
        let api = api_client_with_urls(server.uri(), server.uri(), "");

        Mock::given(method("POST"))
            .and(path("/brainstore/automation/get-cursors"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .mount(&server)
            .await;

        let err = api
            .brainstore_automation()
            .get_cursors(GetAutomationCursorsRequest {
                automation_id: "automation-id".to_string(),
                project_id: "project-id".to_string(),
            })
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            crate::BraintrustError::Api {
                status: 400,
                ref message
            } if message == "bad request"
        ));
    }
}
