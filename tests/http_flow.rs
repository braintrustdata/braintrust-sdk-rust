use braintrust_sdk_rust::{BraintrustClient, BraintrustError, SpanLog};
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

    let client = BraintrustClient::builder()
        .api_key("test-key")
        .user_agent("bt/0.20.0")
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .expect("client");

    let span = client
        .span_builder_with_credentials("secret-token", "org")
        .project_name("demo")
        .build();
    span.log(
        SpanLog::builder()
            .input(Value::String("input".into()))
            .build()
            .expect("build"),
    );
    span.flush().await.expect("flush");
    client.flush().await.expect("client flush");

    let requests = server.received_requests().await.unwrap();
    let logs3 = requests
        .iter()
        .find(|request| request.url.path() == "/logs3")
        .expect("logs3 request");
    let has_expected_user_agent = logs3
        .headers
        .get("user-agent")
        .is_some_and(|value| value.to_str().is_ok_and(|value| value == "bt/0.20.0"));
    assert!(has_expected_user_agent);

    let logs_calls = requests
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

    let client = BraintrustClient::builder()
        .api_key("test-key")
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .expect("client");

    let span = client
        .span_builder_with_credentials("secret-token", "org")
        .project_name("demo")
        .build();

    span.log(
        SpanLog::builder()
            .input(Value::String("input".into()))
            .build()
            .expect("build"),
    );

    // flush() waits for queued work to be processed. Background submission errors
    // are still swallowed by the queue and should not bubble out here.
    let result = span.flush().await;
    assert!(
        result.is_ok(),
        "flush() should not surface background submission errors"
    );

    // Drain pending items before drop so the Drop impl's synchronous flush path
    // is a no-op (queue is empty). The 500 from project registration is swallowed.
    let _ = client.flush().await;
}

#[tokio::test]
async fn checked_flush_reports_backend_rejection() {
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
        .respond_with(ResponseTemplate::new(401).set_body_string("login expired"))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .skip_login(true)
        .checked_delivery(true)
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .unwrap();
    let span = client
        .span_builder_with_credentials("secret-token", "org")
        .project_name("demo")
        .build();
    span.log(SpanLog::builder().input(json!("input")).build().unwrap());
    assert!(matches!(
        client.flush().await,
        Err(BraintrustError::Api { status: 401, .. })
    ));
}

#[tokio::test]
async fn raw_rows_preserve_payload_credentials_and_report_drops() {
    use std::sync::{Arc, Mutex};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .api_url(server.uri())
        .app_url(server.uri())
        .skip_login(true)
        .with_queue_max_size(1)
        .enforce_queue_size_limit(true)
        .build()
        .await
        .unwrap();
    let drops = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&drops);
    client.set_log_drop_handler(Arc::new(move |count, reason| {
        captured.lock().unwrap().push((count, reason))
    }));
    let row = serde_json::from_value::<serde_json::Map<String, Value>>(json!({
        "id": "row-1", "custom_field": {"nested": [1, 2]}, "project_id": "project-1"
    }))
    .unwrap();
    client.log_row_with_credentials(
        "first-token",
        "org-1",
        Some("first-org".into()),
        row.clone(),
    );
    client.log_row_with_credentials("discarded", "org-1", Some("first-org".into()), row.clone());
    assert_eq!(*drops.lock().unwrap(), vec![(1, "queue_full")]);
    client.flush().await.unwrap();
    client.log_row_with_credentials(
        "second-token",
        "org-2",
        Some("second-org".into()),
        row.clone(),
    );
    client.flush().await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let logs = requests
        .iter()
        .filter(|req| req.url.path() == "/logs3")
        .collect::<Vec<_>>();
    assert_eq!(logs.len(), 2);
    for (request, token, org) in [
        (logs[0], "first-token", "first-org"),
        (logs[1], "second-token", "second-org"),
    ] {
        assert_eq!(
            request
                .headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            &format!("Bearer {token}")
        );
        assert_eq!(
            request
                .headers
                .get("x-bt-org-name")
                .unwrap()
                .to_str()
                .unwrap(),
            org
        );
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["rows"], json!([row]));
        assert!(body["api_version"].is_number());
    }
    server.reset().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;
    client.log_row_with_credentials("first-token", "org-1", Some("first-org".into()), row);
    client.flush().await.unwrap();
    assert!(drops.lock().unwrap().contains(&(1, "publish_failed")));
}
