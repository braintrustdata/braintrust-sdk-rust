use braintrust_sdk_rust::{
    configure_service, BraintrustClient, Logs3BatchUploader, ServiceConfig, ServiceRuntime,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn configure_one_slot() {
    configure_service(
        ServiceConfig::builder()
            .max_concurrent_requests(1)
            .runtime(ServiceRuntime::Dedicated)
            .build(),
    )
    .unwrap();
}

#[tokio::test]
async fn clients_share_the_configured_request_limit() {
    configure_one_slot();

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
    for window in times.windows(2) {
        assert!(
            window[1].duration_since(window[0]) >= Duration::from_millis(90),
            "concurrency exceeded the process-wide request limit"
        );
    }
}

#[tokio::test]
async fn signed_uploads_and_queue_requests_share_one_slot() {
    use tokio::sync::Notify;
    use wiremock::matchers::body_string_contains;

    configure_one_slot();
    let server = MockServer::start().await;
    let endpoint = server.uri();
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"logs3_payload_max_bytes": 512})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3/overflow"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "method": "PUT", "signedUrl": format!("{endpoint}/signed"), "headers": {}, "key": "object"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let started = Arc::new(Notify::new());
    let signal = started.clone();
    let arrivals = Arc::new(Mutex::new(Vec::new()));
    let observed = arrivals.clone();
    Mock::given(method("PUT"))
        .and(path("/signed"))
        .respond_with(move |_: &wiremock::Request| {
            observed.lock().unwrap().push(Instant::now());
            signal.notify_one();
            ResponseTemplate::new(200).set_delay(Duration::from_millis(250))
        })
        .expect(1)
        .mount(&server)
        .await;

    let observed = arrivals.clone();
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(body_string_contains("queue-row"))
        .respond_with(move |_: &wiremock::Request| {
            observed.lock().unwrap().push(Instant::now());
            ResponseTemplate::new(200).set_body_string("{}")
        })
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_url(&endpoint)
        .app_url(&endpoint)
        .build_without_login()
        .unwrap();
    let uploader = Logs3BatchUploader::new(&endpoint, "bulk", None).unwrap();
    let upload = uploader.upload_rows(
        &[
            json!({"id":"bulk-row","project_id":"project","input":"x".repeat(8000)})
                .as_object()
                .unwrap()
                .clone(),
        ],
        1,
    );
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    client.log_rows_with_credentials(
        "queue",
        "other-org",
        None,
        [json!({"id":"queue-row","project_id":"project"})
            .as_object()
            .unwrap()
            .clone()],
    );
    assert_eq!(client.flush().await.unwrap().delivered(), 1);
    assert_eq!(upload.await.unwrap().rows_uploaded, 1);

    let times = arrivals.lock().unwrap();
    assert!(times[1].duration_since(times[0]) >= Duration::from_millis(230));
}

#[tokio::test]
async fn terminal_queue_429_pauses_bulk_uploads_for_the_verified_org() {
    use wiremock::matchers::body_string_contains;

    configure_one_slot();
    let server = MockServer::start().await;
    let endpoint = server.uri();
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "org_info": [{"id":"org-id","name":"same-org"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(body_string_contains("limited-row"))
        .respond_with(ResponseTemplate::new(429))
        .with_priority(1)
        .expect(3)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("queue")
        .api_url(&endpoint)
        .app_url(&endpoint)
        .org_name("same-org")
        .blocking_login(true)
        .build()
        .await
        .unwrap();
    client
        .log_rows([json!({"id":"limited-row","project_id":"p"})
            .as_object()
            .unwrap()
            .clone()])
        .await
        .unwrap();
    assert!(client.flush().await.is_err());
    let started = Instant::now();
    let uploader = Logs3BatchUploader::new(&endpoint, "bulk", Some("same-org".into())).unwrap();
    uploader
        .upload_rows(
            &[json!({"id":"bulk-row","project_id":"p"})
                .as_object()
                .unwrap()
                .clone()],
            1,
        )
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(900));
}
