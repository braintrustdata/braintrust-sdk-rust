//! Experiment resource endpoints.
//!
//! This module exposes the low-level `/v1/experiment` API category, including
//! experiment metadata, event fetch, and summary responses. Higher-level
//! experiment logging and feedback workflows live outside this module.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::client::{path_segment, ApiClient};
use crate::api::FetchEventsRequest;

/// Client for experiment API operations.
///
/// Create this through [`crate::ApiClient::experiments()`].
#[derive(Clone, Debug)]
pub struct ExperimentsClient {
    api: ApiClient,
}

impl ExperimentsClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    api_endpoint_methods! {
        list(list, ListExperimentsRequest, ListExperimentsResponse, "v1/experiment", "experiment list response");
        get(get, experiment_id, Experiment, "v1/experiment", "experiment response");
        create(create, CreateExperimentRequest, Experiment, "v1/experiment", "experiment create response");
        post_child(fetch, experiment_id, FetchEventsRequest, FetchExperimentEventsResponse, "v1/experiment", "fetch", "experiment fetch response");
        get_child(summarize, experiment_id, SummarizeExperimentRequest, SummarizeExperimentResponse, "v1/experiment", "summarize", "experiment summarize response");
    }

    /// Deletes one experiment by id.
    pub async fn delete(&self, experiment_id: &str) -> crate::Result<DeleteExperimentResponse> {
        self.api
            .delete_json(
                &format!("v1/experiment/{}", path_segment(experiment_id)),
                "experiment delete response",
            )
            .await
    }
}

/// Request body for creating an experiment.
///
/// Construct with [`CreateExperimentRequest::default()`], then set the fields to send.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct CreateExperimentRequest {
    /// Project id that owns the experiment.
    pub project_id: String,
    /// Optional experiment name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional experiment description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional repository metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_info: Option<Value>,
    /// Optional base experiment id for comparison.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_exp_id: Option<String>,
    /// Optional dataset id associated with the experiment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset_id: Option<String>,
    /// Optional dataset version associated with the experiment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset_version: Option<String>,
    /// Whether the experiment is public.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
    /// Optional experiment metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    /// Optional experiment tags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Whether the backend should create a new experiment instead of reusing one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ensure_new: Option<bool>,
    /// Optional organization name used to disambiguate experiment creation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
}

/// Query parameters for listing experiments.
///
/// Optional fields are omitted from the query string. `ids` is encoded as
/// repeated `ids` query parameters.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ListExperimentsRequest {
    /// Maximum number of experiments to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Pagination cursor for fetching records after a previous result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starting_after: Option<String>,
    /// Pagination cursor for fetching records before a previous result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ending_before: Option<String>,
    /// Experiment ids to include.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
    /// Experiment name filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experiment_name: Option<String>,
    /// Project name filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    /// Project id filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Organization name filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
}

/// Query parameters for summarizing an experiment.
///
/// Set [`SummarizeExperimentRequest::summarize_scores`] to `Some(true)` to
/// request score and metric summary data when the backend supports it.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct SummarizeExperimentRequest {
    /// Whether to include score summary information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summarize_scores: Option<bool>,
    /// Optional experiment id to compare against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison_experiment_id: Option<String>,
}

/// Response returned by [`ExperimentsClient::list()`].
#[derive(Debug, Clone, Deserialize)]
pub struct ListExperimentsResponse {
    objects: Vec<Experiment>,
}

impl ListExperimentsResponse {
    api_getters! {
        slice objects: Experiment;
    }
}

/// Response returned by [`ExperimentsClient::delete()`].
#[derive(Debug, Clone, Deserialize)]
pub struct DeleteExperimentResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    deleted_at: Option<String>,
}

impl DeleteExperimentResponse {
    api_getters! {
        option_str id;
        option_str deleted_at;
    }
}

/// Response returned by [`ExperimentsClient::fetch()`].
///
/// Contains a page of experiment events and an optional cursor for the next
/// page.
#[derive(Debug, Clone, Deserialize)]
pub struct FetchExperimentEventsResponse {
    events: Vec<ExperimentEvent>,
    #[serde(default)]
    cursor: Option<String>,
}

impl FetchExperimentEventsResponse {
    api_getters! {
        slice events: ExperimentEvent;
        option_str cursor;
    }
}

/// Response returned by [`ExperimentsClient::summarize()`].
///
/// Score and metric summaries are keyed by their names. Unknown backend fields
/// are preserved in [`SummarizeExperimentResponse::extra()`].
#[derive(Debug, Clone, Deserialize)]
pub struct SummarizeExperimentResponse {
    project_name: String,
    experiment_name: String,
    project_url: String,
    experiment_url: String,
    #[serde(default)]
    comparison_experiment_name: Option<String>,
    #[serde(default)]
    scores: Option<HashMap<String, ExperimentScoreSummary>>,
    #[serde(default)]
    metrics: Option<HashMap<String, ExperimentMetricSummary>>,
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl SummarizeExperimentResponse {
    api_getters! {
        str project_name;
        str experiment_name;
        str project_url;
        str experiment_url;
        option_str comparison_experiment_name;
        option_hash_map scores: ExperimentScoreSummary;
        option_hash_map metrics: ExperimentMetricSummary;
        map extra;
    }
}

/// Experiment metadata returned by the experiment API.
///
/// Unknown backend fields are preserved in [`Experiment::extra()`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Experiment {
    id: String,
    project_id: String,
    name: String,
    public: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repo_info: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_exp_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deleted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dataset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dataset_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    internal_metadata: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parameters_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parameters_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<String>>,
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl Experiment {
    api_getters! {
        str id;
        str project_id;
        str name;
        bool public;
        option_str description;
        option_str created;
        option_ref repo_info: Value;
        option_str commit;
        option_str base_exp_id;
        option_str deleted_at;
        option_str dataset_id;
        option_str dataset_version;
        option_map internal_metadata;
        option_str parameters_id;
        option_str parameters_version;
        option_str user_id;
        option_map metadata;
        option_slice tags: String;
        map extra;
    }
}

/// Event row returned by [`ExperimentsClient::fetch()`].
///
/// Experiment events expose logged input, output, scores, metrics, and span
/// metadata. Unknown backend fields are preserved in
/// [`ExperimentEvent::extra()`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExperimentEvent {
    id: String,
    #[serde(rename = "_xact_id")]
    xact_id: String,
    created: String,
    project_id: String,
    experiment_id: String,
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

impl ExperimentEvent {
    api_getters! {
        str id;
        str xact_id;
        str created;
        str project_id;
        str experiment_id;
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

/// Aggregate score summary returned by [`ExperimentsClient::summarize()`].
///
/// Unknown backend fields are preserved in [`ExperimentScoreSummary::extra()`].
#[derive(Debug, Clone, Deserialize)]
pub struct ExperimentScoreSummary {
    name: String,
    score: f64,
    #[serde(default)]
    diff: Option<f64>,
    improvements: u64,
    regressions: u64,
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl ExperimentScoreSummary {
    api_getters! {
        str name;
        f64 score;
        option_ref diff: f64;
        u64 improvements;
        u64 regressions;
        map extra;
    }
}

/// Aggregate metric summary returned by [`ExperimentsClient::summarize()`].
///
/// Unknown backend fields are preserved in [`ExperimentMetricSummary::extra()`].
#[derive(Debug, Clone, Deserialize)]
pub struct ExperimentMetricSummary {
    name: String,
    metric: f64,
    unit: String,
    #[serde(default)]
    diff: Option<f64>,
    improvements: u64,
    regressions: u64,
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl ExperimentMetricSummary {
    api_getters! {
        str name;
        f64 metric;
        str unit;
        option_ref diff: f64;
        u64 improvements;
        u64 regressions;
        map extra;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::api::test_support::{
        api_client, assert_query_value, auth_header, json_body, request, request_with_body,
    };

    #[tokio::test]
    async fn methods_send_expected_requests() {
        let server = MockServer::start().await;
        let api = api_client(server.uri());

        Mock::given(method("GET"))
            .and(path("/v1/experiment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "objects": [{
                    "id": "experiment-id",
                    "project_id": "project-id",
                    "name": "Experiment",
                    "public": false
                }]
            })))
            .mount(&server)
            .await;

        let list = api
            .experiments()
            .list(ListExperimentsRequest {
                limit: Some(2),
                experiment_name: Some("Experiment".to_string()),
                project_name: Some("Project".to_string()),
                ..Default::default()
            })
            .await
            .expect("list experiments");
        assert_eq!(list.objects()[0].id(), "experiment-id");

        Mock::given(method("GET"))
            .and(path("/v1/experiment/experiment-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "experiment-id",
                "project_id": "project-id",
                "name": "Experiment",
                "public": false
            })))
            .mount(&server)
            .await;

        let experiment = api
            .experiments()
            .get("experiment-id")
            .await
            .expect("get experiment");
        assert!(!experiment.public());

        Mock::given(method("POST"))
            .and(path("/v1/experiment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "created-experiment-id",
                "project_id": "project-id",
                "name": "Created Experiment",
                "public": true
            })))
            .mount(&server)
            .await;

        let created = api
            .experiments()
            .create(CreateExperimentRequest {
                project_id: "project-id".to_string(),
                name: Some("Created Experiment".to_string()),
                public: Some(true),
                ensure_new: Some(true),
                org_name: Some("Org".to_string()),
                ..Default::default()
            })
            .await
            .expect("create experiment");
        assert_eq!(created.id(), "created-experiment-id");

        Mock::given(method("POST"))
            .and(path("/v1/experiment/experiment-id/fetch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "events": [{
                    "id": "event-id",
                    "_xact_id": "xact-id",
                    "created": "2026-01-01T00:00:00Z",
                    "project_id": "project-id",
                    "experiment_id": "experiment-id",
                    "input": {"question": "2+2"},
                    "output": {"answer": "4"},
                    "scores": {"accuracy": 1},
                    "span_id": "span-id",
                    "root_span_id": "root-span-id"
                }]
            })))
            .mount(&server)
            .await;

        let fetched = api
            .experiments()
            .fetch(
                "experiment-id",
                FetchEventsRequest {
                    limit: Some(20),
                    ..Default::default()
                },
            )
            .await
            .expect("fetch experiment");
        assert_eq!(fetched.events()[0].scores().unwrap()["accuracy"], 1);

        Mock::given(method("GET"))
            .and(path("/v1/experiment/experiment-id/summarize"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "project_name": "Project",
                "experiment_name": "Experiment",
                "project_url": "https://www.braintrust.dev/app/Project",
                "experiment_url": "https://www.braintrust.dev/app/Project/experiments/Experiment",
                "comparison_experiment_name": "Base",
                "scores": {
                    "accuracy": {
                        "name": "accuracy",
                        "score": 0.9,
                        "diff": 0.1,
                        "improvements": 3,
                        "regressions": 1
                    }
                },
                "metrics": {
                    "latency": {
                        "name": "latency",
                        "metric": 42,
                        "unit": "ms",
                        "improvements": 1,
                        "regressions": 0
                    }
                }
            })))
            .mount(&server)
            .await;

        let summary = api
            .experiments()
            .summarize(
                "experiment-id",
                SummarizeExperimentRequest {
                    summarize_scores: Some(true),
                    comparison_experiment_id: Some("base-id".to_string()),
                },
            )
            .await
            .expect("summarize experiment");
        assert_eq!(summary.scores().unwrap()["accuracy"].score(), 0.9);
        assert_eq!(summary.metrics().unwrap()["latency"].unit(), "ms");

        let requests = server.received_requests().await.expect("requests");
        let list = request(&requests, "GET", "/v1/experiment");
        assert_eq!(auth_header(list), "Bearer token");
        assert_query_value(list, "limit", "2");
        assert_query_value(list, "experiment_name", "Experiment");
        assert_query_value(list, "project_name", "Project");

        let create = request_with_body(&requests, "POST", "/v1/experiment");
        assert_eq!(json_body(create)["project_id"], "project-id");
        assert_eq!(json_body(create)["name"], "Created Experiment");
        assert_eq!(json_body(create)["public"], true);
        assert_eq!(json_body(create)["ensure_new"], true);
        assert_eq!(json_body(create)["org_name"], "Org");

        let fetch = request_with_body(&requests, "POST", "/v1/experiment/experiment-id/fetch");
        assert_eq!(json_body(fetch)["limit"], 20);

        let summarize = request(&requests, "GET", "/v1/experiment/experiment-id/summarize");
        assert_query_value(summarize, "summarize_scores", "true");
        assert_query_value(summarize, "comparison_experiment_id", "base-id");
    }

    #[tokio::test]
    async fn delete_sends_expected_request() {
        let server = MockServer::start().await;
        let api = api_client(server.uri());

        Mock::given(method("DELETE"))
            .and(path("/v1/experiment/experiment-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "experiment-id",
                "deleted_at": "2026-01-01T00:00:00Z"
            })))
            .mount(&server)
            .await;

        let deleted = api
            .experiments()
            .delete("experiment-id")
            .await
            .expect("delete");
        assert_eq!(deleted.id(), Some("experiment-id"));

        let requests = server.received_requests().await.expect("requests");
        let request = request(&requests, "DELETE", "/v1/experiment/experiment-id");
        assert_eq!(auth_header(request), "Bearer token");
    }
}
