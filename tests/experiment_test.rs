use braintrust_sdk_rust::{BraintrustClient, ExperimentLog, Feedback};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// Helper to create a mock login response
fn mock_login_response(orgs: &[(&str, &str)]) -> ResponseTemplate {
    let org_info: Vec<_> = orgs
        .iter()
        .map(|(id, name)| json!({ "id": id, "name": name }))
        .collect();
    ResponseTemplate::new(200).set_body_json(json!({ "org_info": org_info }))
}

#[tokio::test]
async fn experiment_log_flushes_to_logs_endpoint() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(mock_login_response(&[("org-id", "Test Org")]))
        .mount(&server)
        .await;

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
        .blocking_login(true)
        .build()
        .await
        .expect("client");

    let experiment = client
        .experiment_builder()
        .await
        .unwrap()
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
                .build()
                .expect("build log"),
        )
        .await;

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
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(mock_login_response(&[("org-id", "Test Org")]))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("token")
        .app_url(server.uri())
        .api_url(server.uri())
        .blocking_login(true)
        .build()
        .await
        .expect("client");

    let result = client
        .experiment_builder()
        .await
        .unwrap()
        .experiment_name("test-experiment")
        .build();

    assert!(result.is_err());
}

#[tokio::test]
async fn experiment_log_feedback_sends_merge_event() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(mock_login_response(&[("org-id", "Test Org")]))
        .mount(&server)
        .await;

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
        .blocking_login(true)
        .build()
        .await
        .expect("client");

    let experiment = client
        .experiment_builder()
        .await
        .unwrap()
        .project_name("test-project")
        .build()
        .expect("build experiment");

    // Log an event
    let id = experiment
        .log(
            ExperimentLog::builder()
                .input(json!("test input"))
                .build()
                .expect("build"),
        )
        .await;

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
        .await;

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
        .and(path("/api/apikey/login"))
        .respond_with(mock_login_response(&[("org-id", "Test Org")]))
        .mount(&server)
        .await;

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
        .blocking_login(true)
        .build()
        .await
        .expect("client");

    let experiment = client
        .experiment_builder()
        .await
        .unwrap()
        .project_name("test-project")
        .experiment_name("my-experiment")
        .build()
        .expect("build experiment");

    // Log an event first
    experiment
        .log(
            ExperimentLog::builder()
                .input(json!("test"))
                .build()
                .expect("build"),
        )
        .await;

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
