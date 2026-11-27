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
}
