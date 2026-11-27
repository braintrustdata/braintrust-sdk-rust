use braintrust_sdk_rust::{configure_service, BraintrustClient, ServiceConfig};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn clients_share_the_configured_request_limit() {
    configure_service(ServiceConfig::builder().max_concurrent_requests(2).build()).unwrap();

    let server = MockServer::start().await;
    let arrivals = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&arrivals);
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(move |_: &wiremock::Request| {
            observed.lock().unwrap().push(Instant::now());
            ResponseTemplate::new(200)
                .set_body_string("{}")
                .set_delay(Duration::from_millis(100))
        })
        .expect(6)
        .mount(&server)
        .await;

    let clients: Vec<_> = (0..6)
        .map(|i| {
            let client = BraintrustClient::builder()
                .api_url(server.uri())
                .app_url(server.uri())
                .checked_delivery(true)
                .build_without_login()
                .unwrap();
            client.log_rows_with_credentials(
                "token",
                &format!("org-{i}"),
                None,
                [json!({"id":i,"project_id":format!("project-{i}")})
                    .as_object()
                    .unwrap()
                    .clone()],
            );
            client
        })
        .collect();

    for result in futures::future::join_all(clients.iter().map(|client| client.flush())).await {
        result.unwrap();
    }
    let times = arrivals.lock().unwrap();
    assert_eq!(times.len(), 6);
    for window in times.windows(3) {
        assert!(
            window[2].duration_since(window[0]) >= Duration::from_millis(90),
            "concurrency exceeded the process-wide request limit"
        );
    }
}
