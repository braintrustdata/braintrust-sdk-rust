//! Project automation resource and internal registration endpoints.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::client::{path_segment, ApiClient};
use crate::api::function_types::SavedFunctionId;

/// Client for project automation API operations.
#[derive(Clone, Debug)]
pub struct ProjectAutomationsClient {
    api: ApiClient,
}

impl ProjectAutomationsClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    /// Lists project automations.
    pub async fn list(
        &self,
        request: ListProjectAutomationsRequest,
    ) -> crate::Result<ListProjectAutomationsResponse> {
        self.api
            .get_json_with_query(
                "v1/project_automation",
                &request,
                "project automation list response",
            )
            .await
    }

    /// Fetches one project automation by id.
    pub async fn get(&self, automation_id: &str) -> crate::Result<ProjectAutomation> {
        self.api
            .get_json(
                &format!("v1/project_automation/{}", path_segment(automation_id)),
                "project automation response",
            )
            .await
    }

    /// Creates a project automation.
    pub async fn create(
        &self,
        request: ProjectAutomationRequest,
    ) -> crate::Result<ProjectAutomation> {
        self.api
            .post_json(
                "v1/project_automation",
                &request,
                "project automation create response",
            )
            .await
    }

    /// Patches one project automation by id.
    pub async fn patch(
        &self,
        automation_id: &str,
        request: ProjectAutomationRequest,
    ) -> crate::Result<ProjectAutomation> {
        self.api
            .patch_json(
                &format!("v1/project_automation/{}", path_segment(automation_id)),
                &request,
                "project automation patch response",
            )
            .await
    }

    /// Deletes one project automation by id.
    pub async fn delete(
        &self,
        automation_id: &str,
    ) -> crate::Result<DeleteProjectAutomationResponse> {
        self.api
            .delete_json(
                &format!("v1/project_automation/{}", path_segment(automation_id)),
                "project automation delete response",
            )
            .await
    }

    /// Registers a project automation through `/api/project_automation/register`.
    pub async fn register(
        &self,
        request: ProjectAutomationRequest,
    ) -> crate::Result<ProjectAutomationRegisterResponse> {
        self.api
            .post_json(
                "api/project_automation/register",
                &request,
                "project automation register response",
            )
            .await
    }

    /// Patches automation ids through `/api/project_automation/patch_id`.
    pub async fn patch_id(
        &self,
        request: ProjectAutomationRequest,
    ) -> crate::Result<ProjectAutomationPatchIdResponse> {
        self.api
            .post_json(
                "api/project_automation/patch_id",
                &request,
                "project automation patch_id response",
            )
            .await
    }

    /// Fetches a topic map report URL.
    pub async fn topic_map_report_url(
        &self,
        request: TopicMapReportUrlRequest,
    ) -> crate::Result<TopicMapReportUrlResponse> {
        self.api
            .post_json(
                "topic-map-report-url",
                &request,
                "topic map report URL response",
            )
            .await
    }
}

/// Query parameters for listing project automations.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ListProjectAutomationsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starting_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ending_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    #[serde(rename = "id", skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
}

/// Request body for project automation create, patch, register, and patch_id.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ProjectAutomationRequest {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        rename = "project_automation_name",
        skip_serializing_if = "Option::is_none"
    )]
    pub project_automation_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ProjectAutomationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update: Option<bool>,
    // TODO: Harden this Value-backed extension map to concrete project automation request fields.
    #[serde(flatten, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// Request body for `/topic-map-report-url`.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct TopicMapReportUrlRequest {
    pub function_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Response returned by [`ProjectAutomationsClient::list()`].
#[derive(Debug, Clone, Deserialize)]
pub struct ListProjectAutomationsResponse {
    #[serde(default)]
    objects: Vec<ProjectAutomation>,
}

impl ListProjectAutomationsResponse {
    api_getters! {
        slice objects: ProjectAutomation;
    }
}

/// Project automation object returned by the API.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ProjectAutomation {
    id: String,
    project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    config: ProjectAutomationConfig,
    // TODO: Harden this Value-backed extension map to concrete project automation fields as they stabilize.
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl ProjectAutomation {
    api_getters! {
        str id;
        str project_id;
        option_str user_id;
        option_str created;
        str name;
        option_str description;
        ref config: ProjectAutomationConfig;
        map extra;
    }
}

/// Response returned by [`ProjectAutomationsClient::delete()`].
#[derive(Debug, Clone, Deserialize)]
pub struct DeleteProjectAutomationResponse {
    #[serde(default)]
    id: Option<String>,
    // TODO: Harden this Value-backed extension map to the concrete project automation delete response schema.
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl DeleteProjectAutomationResponse {
    api_getters! {
        option_str id;
        map extra;
    }
}

/// Response returned by [`ProjectAutomationsClient::register()`].
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ProjectAutomationRegisterResponse {
    project_automation: ProjectAutomation,
    #[serde(default)]
    found_existing: bool,
}

impl ProjectAutomationRegisterResponse {
    api_getters! {
        ref project_automation: ProjectAutomation;
        bool found_existing;
    }
}

/// Response returned by [`ProjectAutomationsClient::patch_id()`].
pub type ProjectAutomationPatchIdResponse = ProjectAutomation;

/// Response returned by [`ProjectAutomationsClient::topic_map_report_url()`].
#[derive(Debug, Clone, Deserialize)]
pub struct TopicMapReportUrlResponse {
    #[serde(default)]
    url: Option<String>,
    // TODO: Harden this Value-backed extension map to concrete topic-map report URL response fields.
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl TopicMapReportUrlResponse {
    api_getters! {
        option_str url;
        map extra;
    }
}

/// Project automation configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProjectAutomationConfig {
    Logs {
        btql_filter: String,
        interval_seconds: f64,
        action: AutomationAction,
    },
    BtqlExport {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<AutomationStatus>,
        export_definition: ExportDefinition,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<AutomationScope>,
        export_path: String,
        format: ExportFormat,
        interval_seconds: f64,
        credentials: ExportCredentials,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        batch_size: Option<f64>,
    },
    Retention {
        object_type: RetentionObjectType,
        retention_days: f64,
    },
    EnvironmentUpdate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        environment_filter: Option<Vec<String>>,
        action: AutomationAction,
    },
    Topic {
        sampling_rate: f64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        facet_functions: Vec<SavedFunctionId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        topic_map_functions: Vec<TopicMapFunctionAutomation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<AutomationScope>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_scope: Option<TopicAutomationDataScope>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        btql_filter: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rerun_seconds: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relabel_overlap_seconds: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backfill_time_range: Option<TopicBackfillTimeRange>,
    },
    #[serde(other)]
    Unknown,
}

/// Automation status.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AutomationStatus {
    Active,
    Paused,
}

/// Action performed by an automation.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AutomationAction {
    Webhook {
        url: String,
    },
    Slack {
        workspace_id: String,
        channel: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_template: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

/// Scope for automation execution.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AutomationScope {
    Span,
    Trace {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idle_seconds: Option<f64>,
    },
    Group {
        group_by: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interval_seconds: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_traces: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placement: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idle_seconds: Option<f64>,
    },
    #[serde(other)]
    Unknown,
}

/// Export definition for BTQL export automation.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExportDefinition {
    LogTraces,
    LogSpans,
    BtqlQuery {
        btql_query: String,
    },
    #[serde(other)]
    Unknown,
}

/// Export output format.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExportFormat {
    Jsonl,
    Parquet,
}

/// Credentials used by BTQL export automation.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExportCredentials {
    AwsIam {
        role_arn: String,
        external_id: String,
    },
    GcpServiceAccount {
        service_account_email: String,
    },
    #[serde(other)]
    Unknown,
}

/// Retention automation object type.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RetentionObjectType {
    ProjectLogs,
    Experiment,
    Dataset,
}

/// Topic map function automation configuration.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[non_exhaustive]
pub struct TopicMapFunctionAutomation {
    pub function: SavedFunctionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btql_filter: Option<String>,
}

/// Data scope for topic automation.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TopicAutomationDataScope {
    ProjectLogs,
    ProjectExperiments,
    Experiment {
        experiment_id: String,
    },
    #[serde(other)]
    Unknown,
}

/// Time range used for topic automation backfills.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum TopicBackfillTimeRange {
    Named(String),
    Range { from: String, to: String },
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

    fn automation_request() -> ProjectAutomationRequest {
        ProjectAutomationRequest {
            project_id: Some("project-id".to_string()),
            name: Some("Automation".to_string()),
            config: Some(ProjectAutomationConfig::Retention {
                object_type: RetentionObjectType::Dataset,
                retention_days: 30.0,
            }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn project_automation_routes_use_api_url_methods_auth_org_and_json() {
        let api_server = MockServer::start().await;
        let app_server = MockServer::start().await;
        let api = api_client_with_urls(api_server.uri(), app_server.uri(), "Org");

        Mock::given(method("GET"))
            .and(path("/v1/project_automation"))
            .and(header("authorization", "Bearer token"))
            .and(header("x-bt-org-name", "Org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "objects": [{
                    "id": "automation-id",
                    "project_id": "project-id",
                    "name": "Automation",
                    "config": {
                        "event_type": "retention",
                        "object_type": "dataset",
                        "retention_days": 30
                    }
                }]
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.project_automations()
                .list(ListProjectAutomationsRequest {
                    limit: Some(10),
                    starting_after: None,
                    ending_before: None,
                    name: Some("Automation".to_string()),
                    project_id: Some("project-id".to_string()),
                    project_name: Some("Project".to_string()),
                    org_name: Some("Org".to_string()),
                    org_id: Some("org-id".to_string()),
                    ids: vec!["automation-id".to_string()],
                })
                .await
                .expect("list")
                .objects()[0]
                .id(),
            "automation-id"
        );

        Mock::given(method("GET"))
            .and(path("/v1/project_automation/automation-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "automation-id",
                "project_id": "project-id",
                "name": "Automation",
                "config": {
                    "event_type": "retention",
                    "object_type": "dataset",
                    "retention_days": 30
                }
            })))
            .mount(&api_server)
            .await;
        api.project_automations()
            .get("automation-id")
            .await
            .expect("get");

        Mock::given(method("POST"))
            .and(path("/v1/project_automation"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "created-automation-id",
                "project_id": "project-id",
                "name": "Automation",
                "config": {
                    "event_type": "retention",
                    "object_type": "dataset",
                    "retention_days": 30
                }
            })))
            .mount(&api_server)
            .await;
        api.project_automations()
            .create(automation_request())
            .await
            .expect("create");

        Mock::given(method("PATCH"))
            .and(path("/v1/project_automation/automation-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "automation-id",
                "project_id": "project-id",
                "name": "Patched",
                "config": {
                    "event_type": "retention",
                    "object_type": "dataset",
                    "retention_days": 30
                }
            })))
            .mount(&api_server)
            .await;
        api.project_automations()
            .patch("automation-id", automation_request())
            .await
            .expect("patch");

        Mock::given(method("DELETE"))
            .and(path("/v1/project_automation/automation-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "automation-id"
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.project_automations()
                .delete("automation-id")
                .await
                .expect("delete")
                .id(),
            Some("automation-id")
        );

        Mock::given(method("POST"))
            .and(path("/api/project_automation/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "project_automation": {
                    "id": "automation-id",
                    "project_id": "project-id",
                    "name": "Automation",
                    "config": {
                        "event_type": "retention",
                        "object_type": "dataset",
                        "retention_days": 30
                    }
                },
                "found_existing": true
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.project_automations()
                .register(automation_request())
                .await
                .expect("register")
                .found_existing(),
            true
        );

        Mock::given(method("POST"))
            .and(path("/api/project_automation/patch_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "automation-id",
                "project_id": "project-id",
                "name": "Automation",
                "config": {
                    "event_type": "retention",
                    "object_type": "dataset",
                    "retention_days": 30
                }
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.project_automations()
                .patch_id(automation_request())
                .await
                .expect("patch_id")
                .id(),
            "automation-id"
        );

        Mock::given(method("POST"))
            .and(path("/topic-map-report-url"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "url": "https://example.test/report"
            })))
            .mount(&api_server)
            .await;
        assert_eq!(
            api.project_automations()
                .topic_map_report_url(TopicMapReportUrlRequest {
                    function_id: "function-id".to_string(),
                    version: Some("version-id".to_string()),
                })
                .await
                .expect("topic map")
                .url(),
            Some("https://example.test/report")
        );

        let requests = api_server.received_requests().await.expect("requests");
        let list = request(&requests, "GET", "/v1/project_automation");
        assert_query_value(list, "limit", "10");
        assert_query_value(list, "name", "Automation");
        assert_query_value(list, "project_id", "project-id");
        assert_query_value(list, "project_name", "Project");
        assert_query_value(list, "org_name", "Org");
        assert_query_value(list, "org_id", "org-id");
        assert_eq!(query_values(list, "id"), vec!["automation-id"]);

        let create = request_with_body(&requests, "POST", "/v1/project_automation");
        assert_eq!(auth_header(create), "Bearer token");
        assert_eq!(json_body(create)["project_id"], "project-id");

        let report = request_with_body(&requests, "POST", "/topic-map-report-url");
        assert_eq!(json_body(report)["function_id"], "function-id");
        assert_eq!(json_body(report)["version"], "version-id");

        let app_requests = app_server.received_requests().await.expect("app requests");
        assert!(app_requests.is_empty());
    }

    #[tokio::test]
    async fn project_automation_non_success_status_maps_to_error() {
        let server = MockServer::start().await;
        let api = api_client_with_urls(server.uri(), server.uri(), "");

        Mock::given(method("GET"))
            .and(path("/v1/project_automation"))
            .respond_with(ResponseTemplate::new(500).set_body_string("failed"))
            .mount(&server)
            .await;

        let err = api
            .project_automations()
            .list(ListProjectAutomationsRequest::default())
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            crate::BraintrustError::Api {
                status: 500,
                ref message
            } if message == "failed"
        ));
    }
}
