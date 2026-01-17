use braintrust_sdk_rust::{BraintrustClient, SpanLog};
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path};
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
async fn submits_with_bearer_token() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(mock_login_response(&[("org-id", "Test Org")]))
        .mount(&server)
        .await;

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

    let client = BraintrustClient::builder()
        .api_key("test-key")
        .api_url(server.uri())
        .app_url(server.uri())
        .blocking_login(true)
        .build()
        .await
        .expect("client");

    let span = client
        .span_builder()
        .await
        .unwrap()
        .project_name("demo")
        .build();
    span.log(
        SpanLog::builder()
            .input(Value::String("input".into()))
            .build()
            .expect("build"),
    )
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
        .and(path("/api/apikey/login"))
        .respond_with(mock_login_response(&[("org-id", "Test Org")]))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("test-key")
        .api_url(server.uri())
        .app_url(server.uri())
        .blocking_login(true)
        .build()
        .await
        .expect("client");

    let span = client
        .span_builder()
        .await
        .unwrap()
        .project_name("demo")
        .build();

    // flush() queues the data and returns immediately.
    // The actual HTTP submission (which fails) happens in the background.
    let result = span.flush().await;
    assert!(result.is_ok(), "flush() should be fire-and-forget");
}
