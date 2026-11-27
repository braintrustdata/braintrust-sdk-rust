mod support;

use support::MockServer;

use braintrust_sdk_rust::{BraintrustClient, ExperimentLog, Feedback};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test]
async fn experiment_log_flushes_to_logs_endpoint() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": { "id": "proj-id", "name": "test-project" },
            "experiment": { "id": "exp-id", "name": "test-experiment" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("token")
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .expect("client");

    let experiment = client
        .experiment_builder_with_credentials("token", "org-id")
        .project_name("test-project")
        .experiment_name("test-experiment")
        .build()
        .expect("build experiment");

    let _id = experiment
        .log(
            ExperimentLog::builder()
                .input(json!({"question": "What is 2+2?"}))
                .output(json!({"answer": "4"}))
                .expected(json!({"answer": "4"}))
                .score("accuracy", 1.0)
                .build(),
        )
        .await
        .unwrap();

    experiment.flush().await.expect("flush");
    client.flush().await.expect("client flush");

    let logs_requests: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/logs3")
        .collect();
    assert!(!logs_requests.is_empty(), "expected logs request");

    let body: Value = serde_json::from_slice(&logs_requests[0].body).expect("json body");
    assert_eq!(body["api_version"], 2);
    let row = body["rows"]
        .as_array()
        .and_then(|rows| rows.first())
        .expect("row");
    assert_eq!(row["experiment_id"], "exp-id");
}

#[tokio::test]
async fn experiment_builder_requires_project_name() {
    support::use_dedicated_runtime();
    let client = BraintrustClient::builder()
        .api_key("token")
        .api_url("https://api.braintrust.dev")
        .build()
        .await
        .expect("client");

    let result = client
        .experiment_builder_with_credentials("token", "org-id")
        .experiment_name("test-experiment")
        .build();

    assert!(result.is_err());
}

#[tokio::test]
async fn experiment_log_feedback_sends_merge_event() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": { "id": "proj-id", "name": "test-project" },
            "experiment": { "id": "exp-id", "name": "test-experiment" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("token")
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .expect("client");

    let experiment = client
        .experiment_builder_with_credentials("token", "org-id")
        .project_name("test-project")
        .build()
        .expect("build experiment");

    // Log an event
    let id = experiment
        .log(ExperimentLog::builder().input(json!("test input")).build())
        .await
        .unwrap();

    // Log feedback on the event
    experiment
        .log_feedback(
            &id,
            Feedback::builder()
                .score("human_rating", 0.9)
                .comment("Good response")
                .build()
                .expect("build feedback"),
        )
        .await
        .unwrap();

    experiment.flush().await.expect("flush");
    client.flush().await.expect("client flush");

    // Verify at least one logs request was made
    let logs_requests: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/logs3")
        .collect();
    assert!(!logs_requests.is_empty(), "expected logs requests");
}

#[tokio::test]
async fn experiment_summarize_returns_summary() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": { "id": "proj-id", "name": "test-project" },
            "experiment": { "id": "exp-id", "name": "test-experiment" }
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    // Mock base experiment fetch - return 400 (no base experiment)
    Mock::given(method("POST"))
        .and(path("/api/base_experiment/get_id"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "No base experiment"
        })))
        .mount(&server)
        .await;

    // Mock experiment comparison
    Mock::given(method("GET"))
        .and(path("/experiment-comparison2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "scores": {
                "accuracy": {
                    "score": 0.95,
                    "diff": 0.05,
                    "improvements": 10,
                    "regressions": 2
                }
            },
            "metrics": {}
        })))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("token")
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .expect("client");

    let experiment = client
        .experiment_builder_with_credentials("token", "org-id")
        .project_name("test-project")
        .experiment_name("my-experiment")
        .build()
        .expect("build experiment");

    // Log an event first
    experiment
        .log(ExperimentLog::builder().input(json!("test")).build())
        .await
        .unwrap();

    let summary = experiment.summarize().await.expect("summarize");

    assert_eq!(summary.project_name(), "test-project");
    assert_eq!(summary.experiment_name(), "test-experiment");
    assert_eq!(summary.project_id(), Some("proj-id"));
    assert_eq!(summary.experiment_id(), Some("exp-id"));

    // Verify scores from comparison API
    let scores = summary.scores();
    assert!(scores.contains_key("accuracy"));
    let accuracy_score = scores.get("accuracy").unwrap();
    assert_eq!(accuracy_score.score(), 0.95);
    assert_eq!(accuracy_score.diff(), Some(0.05));
    assert_eq!(accuracy_score.improvements(), Some(10));
    assert_eq!(accuracy_score.regressions(), Some(2));
}

async fn experiment_server(register: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(register)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    server
}
fn registered() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "project": { "id": "proj-id", "name": "test-project" },
        "experiment": { "id": "exp-id", "name": "test-experiment" }
    }))
}
async fn uploaded_rows(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/logs3")
        .flat_map(|request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            body["rows"].as_array().unwrap().clone()
        })
        .collect()
}

#[tokio::test]
async fn feedback_is_not_overwritten_by_a_later_flush() {
    let server = experiment_server(registered()).await;
    let client = BraintrustClient::builder()
        .app_url(server.uri())
        .api_url(server.uri())
        .build_without_login()
        .unwrap();
    let experiment = client
        .experiment_builder_with_credentials("token", "org-id")
        .project_name("test-project")
        .build()
        .unwrap();
    let id = experiment
        .log(ExperimentLog::builder().score("accuracy", 0.2).build())
        .await
        .unwrap();
    experiment
        .log_feedback(
            &id,
            Feedback::builder().score("accuracy", 0.9).build().unwrap(),
        )
        .await
        .unwrap();
    experiment.flush().await.unwrap();
    let scores: Vec<_> = uploaded_rows(&server)
        .await
        .into_iter()
        .filter_map(|row| row["scores"]["accuracy"].as_f64())
        .collect();
    assert_eq!(scores.last(), Some(&0.9), "uploaded scores: {scores:?}");
}

#[tokio::test]
async fn flush_covers_writes_started_before_it_even_if_not_awaited() {
    let server = experiment_server(registered()).await;
    let client = BraintrustClient::builder()
        .app_url(server.uri())
        .api_url(server.uri())
        .build_without_login()
        .unwrap();
    let experiment = client
        .experiment_builder_with_credentials("token", "org-id")
        .project_name("test-project")
        .build()
        .unwrap();
    // Started, never awaited; the write first waits for registration.
    drop(experiment.log(ExperimentLog::builder().input(json!("not awaited")).build()));
    experiment.flush().await.unwrap();
    let rows = uploaded_rows(&server).await;
    assert!(
        rows.iter().any(|row| row["input"] == "not awaited"),
        "{rows:?}"
    );
}

#[tokio::test]
async fn a_failed_registration_is_not_retried_by_every_write() {
    let server = experiment_server(ResponseTemplate::new(500)).await;
    let client = BraintrustClient::builder()
        .app_url(server.uri())
        .api_url(server.uri())
        .build_without_login()
        .unwrap();
    let experiment = client
        .experiment_builder_with_credentials("token", "org-id")
        .project_name("test-project")
        .build()
        .unwrap();
    for _ in 0..3 {
        experiment
            .log(ExperimentLog::builder().input(json!("row")).build())
            .await
            .unwrap();
    }
    let registrations = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/api/experiment/register")
        .count();
    assert_eq!(registrations, 1);
}
