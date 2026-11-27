use braintrust_sdk_rust::BraintrustClient;
use serde_json::{json, Map, Value};
use std::time::Duration;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn row(id: &str) -> Map<String, Value> {
    json!({"id":id,"project_id":"project","log_id":"g"})
        .as_object()
        .unwrap()
        .clone()
}

async fn credentialed_row_upload_stops_at_the_first_end_of_input() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .api_key("test-key")
        .api_url(server.uri())
        .skip_login(true)
        .build()
        .await
        .unwrap();
    let rows = [Some(row("first")), None, Some(row("after-end"))]
        .into_iter()
        .scan((), |_, row| row);

    client.log_rows_with_credentials("test-key", "org-id", None, rows);
    assert_eq!(client.flush().await.unwrap().delivered(), 1);
}

#[tokio::test]
async fn flush_and_shutdown_cover_writes_waiting_for_login() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_json(json!({"org_info":[{"id":"org-id","name":"org"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .app_url(server.uri())
        .api_url(server.uri())
        .api_key("test-key")
        .build()
        .await
        .unwrap();

    drop(client.log_rows([row("covered")]));
    let flush = client.flush();
    let shutdown = client.shutdown(Duration::from_secs(5));
    assert!(client.log_rows([row("late")]).await.is_err());
    assert_eq!(flush.await.unwrap().delivered(), 1);
    assert_eq!(shutdown.await.unwrap().delivered(), 1);

    let requests = server.received_requests().await.unwrap();
    let body = requests
        .iter()
        .find(|request| request.url.path() == "/logs3")
        .unwrap();
    let body: Value = serde_json::from_slice(&body.body).unwrap();
    assert_eq!(body["rows"].as_array().unwrap().len(), 1);
    assert_eq!(body["rows"][0]["id"], "covered");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(1))
                .set_body_json(json!({"org_info":[{"id":"org-id","name":"org"}]})),
        )
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .app_url(server.uri())
        .api_url(server.uri())
        .api_key("test-key")
        .build()
        .await
        .unwrap();
    let write = client.log_rows([row("cancelled")]);
    let flush = client.flush();
    assert!(client.shutdown(Duration::from_millis(10)).await.is_err());
    assert!(tokio::time::timeout(Duration::from_millis(300), write)
        .await
        .unwrap()
        .is_err());
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(300), flush)
            .await
            .unwrap(),
        Err(braintrust_sdk_rust::FlushError::Execution(
            braintrust_sdk_rust::ExecutionError::TimedOut
        ))
    ));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_json(json!({"org_info":[{"id":"org-id","name":"org"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .app_url(server.uri())
        .api_url(server.uri())
        .api_key("test-key")
        .build()
        .await
        .unwrap();
    drop(client.log_rows([row("first-failure")]));
    drop(client.log_rows([row("second-failure")]));
    let error = client.flush().await.unwrap_err();
    assert!(
        matches!(error, braintrust_sdk_rust::FlushError::Delivery(failure) if failure.failed() == 2)
    );

    credentialed_row_upload_stops_at_the_first_end_of_input().await;
}
