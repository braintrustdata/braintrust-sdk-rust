use braintrust_sdk_rust::{BraintrustClient, BraintrustClientConfig, SpanLog};
use serde_json::{json, Value};
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
    span.log(SpanLog {
        input: Some(Value::String("input".into())),
        ..Default::default()
    })
    .await;
    span.flush().await.expect("flush");
    client.flush().await.expect("client flush");

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
async fn flush_is_fire_and_forget() {
    // flush() should succeed even when project registration would fail.
    // Errors are logged but not propagated (resilient design).
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

    // flush() queues the data and returns immediately.
    // The actual HTTP submission (which fails) happens in the background.
    let result = span.flush().await;
    assert!(result.is_ok(), "flush() should be fire-and-forget");
}
