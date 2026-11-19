//! Small shared fixtures. Mock values and client builders remain directly customizable.
// Each integration test is a separate crate and uses a different subset.
#![allow(dead_code)]

use braintrust_sdk_rust::{
    BraintrustClient, BraintrustClientBuilder, Experiment, ParentSpanInfo, SpanHandle,
};
use serde_json::{json, Value};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, Request, ResponseTemplate,
};

/// Skip login against local endpoints; runtime selection stays at the call site.
pub fn client_builder(uri: impl Into<String>) -> BraintrustClientBuilder {
    endpoints(uri).skip_login(true)
}

/// Bind to the test runtime so synchronous issuance cannot race queue consumption.
pub async fn bound_client(server: &MockServer, capacity: usize) -> BraintrustClient {
    client_builder(server.uri())
        .runtime(braintrust_sdk_rust::current_runtime())
        .with_queue_max_size(capacity)
        .build()
        .await
        .unwrap()
}

pub fn span(client: &BraintrustClient) -> SpanHandle {
    client
        .span_builder_with_credentials("token", "org")
        .parent_info(ParentSpanInfo::ProjectLogs {
            object_id: "project".into(),
        })
        .build()
}

pub fn experiment(client: &BraintrustClient) -> Experiment {
    client
        .experiment_builder_with_credentials("token", "org")
        .project_name("project")
        .build()
}

pub fn experiment_response() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "project":{"id":"project","name":"project"},
        "experiment":{"id":"experiment","name":"experiment"}
    }))
}

pub fn logs(status: u16) -> Mock {
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(status))
}

pub fn logs_json() -> Mock {
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
}

pub fn version() -> Mock {
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
}

pub async fn ingestion(server: &MockServer) {
    version().mount(server).await;
    logs(204).mount(server).await;
}

pub fn log_requests(requests: &[Request]) -> impl Iterator<Item = &Request> {
    requests.iter().filter(|r| r.url.path() == "/logs3")
}

pub async fn received_logs(server: &MockServer) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/logs3")
        .collect()
}

pub fn body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("JSON request body")
}

/// Set only endpoint URLs; login behavior is always explicit for login tests.
pub fn endpoints(uri: impl Into<String>) -> BraintrustClientBuilder {
    let uri = uri.into();
    BraintrustClient::builder().api_url(&uri).app_url(&uri)
}

pub async fn client(server: &MockServer) -> BraintrustClient {
    client_builder(server.uri()).build().await.unwrap()
}

pub fn login() -> Mock {
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "org_info":[{"id":"org","name":"tenant"}]
        })))
}

pub fn register_experiment() -> Mock {
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(experiment_response())
}

pub fn log_rows(requests: &[Request]) -> Vec<Value> {
    log_requests(requests)
        .flat_map(|r| body(r)["rows"].as_array().expect("rows").clone())
        .collect()
}

pub fn project_context() -> braintrust_sdk_rust::SpanComponents {
    use braintrust_sdk_rust::{SpanComponents, SpanObjectType};
    SpanComponents {
        object_type: SpanObjectType::ProjectLogs,
        object_id: Some("proj-id".into()),
        compute_object_metadata_args: None,
        row_id: Some("row-id".into()),
        span_id: Some("span-id".into()),
        root_span_id: Some("root-id".into()),
        span_parents: None,
        propagated_event: None,
    }
}
