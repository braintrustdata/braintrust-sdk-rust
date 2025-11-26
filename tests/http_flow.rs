use braintrust_sdk_rust::{BraintrustClient, BraintrustClientConfig};
use serde_json::json;
use serde_json::Value;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn submits_with_bearer_token() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "project": { "id": "proj" }
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let client = BraintrustClient::new(BraintrustClientConfig::new(server.uri())).expect("client");

    let span = client
        .span_builder("secret-token", "org")
        .project_name("demo")
        .build();
    span.log_input(Value::String("input".into())).await;
    span.finish().await.expect("finish");
    client.flush().await.expect("flush");

    let logs_calls = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|req| req.url.path() == "/logs3")
        .count();
    assert_eq!(logs_calls, 1);
}

#[tokio::test]
async fn project_registration_failure_bubbles_up() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let client = BraintrustClient::new(BraintrustClientConfig::new(server.uri())).expect("client");

    let span = client
        .span_builder("secret-token", "org")
        .project_name("demo")
        .build();

    let result = span.finish().await;
    assert!(result.is_err());
}

