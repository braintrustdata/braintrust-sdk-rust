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
async fn preparation_failure_reaches_write_and_covering_flush() {
    // Preparation failure terminates the logical batch, then is forgotten.
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

    let completion = span.log(
        SpanLog::builder()
            .input(Value::String("input".into()))
            .build()
            .expect("build"),
    );

    assert!(span.flush().await.is_err());
    assert!(matches!(
        completion.await,
        Err(braintrust_sdk_rust::WriteError::Delivery(_))
    ));
    client
        .flush()
        .await
        .expect("old failures must not poison a later flush");
}

#[tokio::test]
async fn automatic_delivery_reports_backend_rejection_through_completion() {
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
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .unwrap();
    let span = client
        .span_builder_with_credentials("secret-token", "org")
        .project_name("demo")
        .build();
    let completion = span.log(SpanLog::builder().input(json!("input")).build().unwrap());
    assert!(
        matches!(completion.await, Err(braintrust_sdk_rust::WriteError::Delivery(error))
        if matches!(error.cause().downcast_ref::<BraintrustError>(), Some(BraintrustError::Api { status: 401, .. })))
    );
    client
        .flush()
        .await
        .expect("completed background failure is retired");
}

#[tokio::test]
async fn submission_and_flush_can_be_issued_without_a_call_site_runtime() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"project":{"id":"proj"}})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .skip_login(true)
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .unwrap();
    let span = client
        .span_builder_with_credentials("token", "org")
        .project_name("demo")
        .build();
    let (write, flush) = std::thread::spawn(move || {
        let write = span.log(SpanLog::builder().input(json!("input")).build().unwrap());
        let flush = client.flush();
        assert!(write.is_ok());
        assert!(flush.is_ok());
        (write, flush)
    })
    .join()
    .unwrap();
    write.await.unwrap();
    assert_eq!(flush.await.unwrap().delivered(), 1);
}
