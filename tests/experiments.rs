use braintrust_sdk_rust::{BraintrustClient, ExperimentLog, Feedback};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
        .expect("log event");

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
    let client = BraintrustClient::builder()
        .skip_login(true)
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
        .expect("log event");

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
        .expect("log event");

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
        .expect("log event");

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

async fn isolated_client(server: &MockServer) -> BraintrustClient {
    BraintrustClient::builder()
        .skip_login(true)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap()
}
fn registered_experiment() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"project":{"id":"project","name":"project"},"experiment":{"id":"experiment","name":"experiment"}}))
}
fn assert_api_error<T>(result: Result<T, braintrust_sdk_rust::BraintrustError>, expected: u16) {
    assert!(
        matches!(result, Err(braintrust_sdk_rust::BraintrustError::Api {status, ..}) if status == expected)
    );
}

#[tokio::test]
async fn registration_failure_has_no_invented_id_and_clones_share_a_successful_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(registered_experiment())
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let experiment = client
        .experiment_builder_with_credentials("token", "org")
        .project_name("project")
        .build()
        .unwrap();
    let clone = experiment.clone();
    assert_api_error(experiment.project().await, 503);
    assert_eq!(experiment.experiment_id(), None);
    assert_eq!(clone.experiment_id(), None);
    let (first, second) = tokio::join!(experiment.project(), clone.project());
    assert_eq!(first.unwrap().id, "project");
    assert_eq!(second.unwrap().id, "project");
    assert_eq!(experiment.experiment_id().as_deref(), Some("experiment"));
    assert_eq!(clone.experiment_id().as_deref(), Some("experiment"));
    assert_eq!(
        experiment.clone().export().await.unwrap().object_id,
        "experiment"
    );
    experiment
        .log(ExperimentLog::builder().input(json!("value")).build())
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(
        &requests
            .iter()
            .find(|req| req.url.path() == "/logs3")
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(body["rows"][0]["experiment_id"], "experiment");
    assert!(body["rows"][0]["metrics"]["end"].is_number());
}

#[tokio::test]
async fn every_registration_dependent_operation_returns_the_real_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(ResponseTemplate::new(400))
        .expect(9)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let experiment = client
        .experiment_builder_with_credentials("token", "org")
        .project_name("project")
        .build()
        .unwrap();
    assert_api_error(experiment.start_span().await, 400);
    assert_api_error(
        experiment
            .log(ExperimentLog::builder().input(json!("value")).build())
            .await,
        400,
    );
    assert_api_error(experiment.project().await, 400);
    assert_api_error(experiment.export().await, 400);
    assert_api_error(
        experiment
            .log_feedback("row", Feedback::builder().build().unwrap())
            .await,
        400,
    );
    assert_api_error(
        experiment
            .update_span("row", braintrust_sdk_rust::SpanLog::default())
            .await,
        400,
    );
    assert_api_error(experiment.fetch_base_experiment().await, 400);
    assert_api_error(experiment.summarize().await, 400);
    let called = std::sync::atomic::AtomicBool::new(false);
    assert_api_error(
        experiment
            .traced(|_| async {
                called.store(true, std::sync::atomic::Ordering::Relaxed);
                42
            })
            .await,
        400,
    );
    assert!(!called.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(experiment.experiment_id(), None);
}

#[tokio::test]
async fn event_feedback_and_update_methods_propagate_delivery_rejection() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(registered_experiment())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(401))
        .expect(3)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let experiment = client
        .experiment_builder_with_credentials("token", "org")
        .project_name("project")
        .build()
        .unwrap();
    assert_api_error(
        experiment
            .log(ExperimentLog::builder().input(json!("value")).build())
            .await,
        401,
    );
    assert_api_error(
        experiment
            .log_feedback("row", Feedback::builder().build().unwrap())
            .await,
        401,
    );
    assert_api_error(
        experiment
            .update_span("row", braintrust_sdk_rust::SpanLog::default())
            .await,
        401,
    );
    experiment.flush().await.unwrap(); // completed failures were already observed and retired
}

#[tokio::test]
async fn comparison_setup_errors_are_not_silently_treated_as_no_base_experiment() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(registered_experiment())
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/base_experiment/get_id"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/experiment-comparison2"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let experiment = client
        .experiment_builder_with_credentials("token", "org")
        .project_name("project")
        .build()
        .unwrap();
    assert_api_error(experiment.summarize().await, 401);
}

#[tokio::test]
async fn explicit_experiment_credentials_never_prime_the_shared_session() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(registered_experiment())
        .expect(1)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let state = client.login_state().await;
    let experiment = client
        .experiment_builder_with_credentials("private-token", "tenant")
        .project_name("project")
        .build()
        .unwrap();
    assert!(!state.is_logged_in());
    experiment.project().await.unwrap();
    assert!(!state.is_logged_in());
    assert!(!client.is_logged_in().await);
    assert!(client.dataset_builder().await.is_err());
    let exported = experiment.export().await.unwrap();
    assert_eq!(exported.object_id, "experiment");
    assert!(!state.is_logged_in());
}

#[tokio::test]
async fn traced_propagates_final_flush_errors_instead_of_discarding_them() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(registered_experiment())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let experiment = client
        .experiment_builder_with_credentials("token", "org")
        .project_name("project")
        .build()
        .unwrap();
    assert_api_error(experiment.traced(|_| async { 42 }).await, 401);
}

#[tokio::test]
async fn clones_share_the_selected_span_recording_interface() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(registered_experiment())
        .expect(1)
        .mount(&server)
        .await;
    let client = isolated_client(&server).await;
    let experiment = client
        .experiment_builder_with_credentials("token", "org")
        .project_name("project")
        .build()
        .unwrap();
    let clone = experiment.clone();
    experiment.start_span().await.unwrap();
    assert!(matches!(
        clone
            .log(ExperimentLog::builder().input(json!("value")).build())
            .await,
        Err(braintrust_sdk_rust::BraintrustError::InvalidConfig(_))
    ));
}
