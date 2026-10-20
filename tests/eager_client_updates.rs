use braintrust_sdk_rust::{
    BraintrustClient, BraintrustError, EnqueueError, ExecutionError, FlushError, ParentSpanInfo,
    ShutdownError, SpanComponents, SpanLog, SpanObjectType, WriteError,
};
use serde_json::{json, Value};
use std::{future::IntoFuture, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::{
    matchers::{header, method, path},
    Match, Mock, MockServer, ResponseTemplate,
};

fn exported(id: &str) -> String {
    SpanComponents {
        object_type: SpanObjectType::ProjectLogs,
        object_id: Some("project".into()),
        compute_object_metadata_args: None,
        row_id: Some(id.into()),
        span_id: Some(format!("span-{id}")),
        root_span_id: Some(format!("root-{id}")),
        span_parents: None,
        propagated_event: None,
    }
    .to_str()
}
async fn ingestion(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .mount(server)
        .await;
}
async fn client(server: &MockServer, capacity: usize) -> BraintrustClient {
    BraintrustClient::builder()
        .runtime(braintrust_sdk_rust::current_runtime())
        .skip_login(true)
        .app_url(server.uri())
        .api_url(server.uri())
        .with_queue_max_size(capacity)
        .build()
        .await
        .unwrap()
}

// Keep login unresolved until the test has issued update, flush and shutdown.
// This async HTTP gate avoids relying on response latency or blocking a runtime.
async fn gated_login(
    body: Value,
) -> (
    String,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}", listener.local_addr().unwrap());
    let (arrived, arrival) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            assert!(stream.read_buf(&mut request).await.unwrap() > 0);
        }
        assert!(request.starts_with(b"POST /api/apikey/login "));
        arrived.send(()).unwrap();
        released.await.unwrap();
        let body = body.to_string();
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
    });
    (uri, arrival, release, task)
}

#[tokio::test]
async fn plain_thread_explicit_updates_progress_unobserved_and_own_exported_context() {
    let server = MockServer::start().await;
    ingestion(&server).await;
    let uri = server.uri();
    let (flush, shutdown) = std::thread::spawn(move || {
        let client = futures::executor::block_on(
            BraintrustClient::builder()
                .skip_login(true)
                .app_url(&uri)
                .api_url(&uri)
                .build()
                .into_future(),
        )
        .unwrap();
        let update = client.update_span_with_credentials(
            "explicit",
            "org",
            &exported("row"),
            SpanLog::builder()
                .output(json!("accepted"))
                .build()
                .unwrap(),
        );
        assert!(update.is_ok());
        assert!(!client.is_logged_in());
        drop(update);
        let flush = client.flush();
        let shutdown = client.shutdown(Duration::from_secs(5));
        drop(client);
        (flush, shutdown)
    })
    .join()
    .unwrap();
    assert_eq!(flush.await.unwrap().delivered, 1);
    shutdown.await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let request = requests.iter().find(|r| r.url.path() == "/logs3").unwrap();
    assert!(header("authorization", "Bearer explicit").matches(request));
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    let row = &body["rows"][0];
    assert_eq!(row["id"], "row");
    assert_eq!(row["span_id"], "span-row");
    assert_eq!(row["root_span_id"], "root-row");
    assert_eq!(row["org_id"], "org");
    assert_eq!(row["project_id"], "project");
    assert_eq!(row["output"], "accepted");
    assert_eq!(row["_is_merge"], true);
    assert!(requests.iter().all(|r| r.url.path() != "/api/apikey/login"));
}

#[tokio::test]
async fn pending_login_update_captures_shutdown_and_uses_the_selected_org_route() {
    let configured = MockServer::start().await;
    let selected = MockServer::start().await;
    ingestion(&selected).await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(header("authorization", "Bearer session"))
        .respond_with(ResponseTemplate::new(204))
        .with_priority(1)
        .expect(1)
        .mount(&selected)
        .await;
    let (control, arrived, release, login) = gated_login(json!({
        "org_info":[{"id":"selected-org","name":"tenant","api_url":selected.uri()}]
    }))
    .await;
    let client = BraintrustClient::builder()
        .runtime(braintrust_sdk_rust::current_runtime())
        .api_key("session")
        .blocking_login(false)
        .app_url(control)
        .api_url(configured.uri())
        .build()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), arrived)
        .await
        .unwrap()
        .unwrap();
    assert!(!client.is_logged_in());
    let update = client.update_span(&exported("pending"), SpanLog::default());
    assert!(update.is_ok());
    let flush = client.flush();
    let shutdown = client.shutdown(Duration::from_secs(5));
    release.send(()).unwrap();
    assert_eq!(shutdown.await.unwrap().delivered, 1);
    assert_eq!(flush.await.unwrap().delivered, 1);
    update.await.unwrap();
    login.await.unwrap();
    assert!(configured.received_requests().await.unwrap().is_empty());
    let requests = selected.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(
        &requests
            .iter()
            .find(|r| r.url.path() == "/logs3")
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(body["rows"][0]["org_id"], "selected-org");
    assert_eq!(body["rows"][0]["org_name"], "tenant");
    assert!(matches!(
        &*client.update_span(&exported("closed"), SpanLog::default()),
        Err(BraintrustError::Admission(EnqueueError::Closed))
    ));
}

#[tokio::test]
async fn delivery_failure_resolves_update_and_covering_flush_then_fresh_work_succeeds() {
    let server = MockServer::start().await;
    ingestion(&server).await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(400))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server, 10).await;
    let update = client.update_span_with_credentials(
        "token",
        "org",
        &exported("failed"),
        SpanLog::default(),
    );
    let neighbor = client
        .span_builder_with_credentials("token", "org")
        .parent_info(ParentSpanInfo::ProjectLogs {
            object_id: "project".into(),
        })
        .build()
        .end();
    let flush = client.flush();
    assert!(update.is_ok());
    assert!(matches!(
        update.await,
        Err(BraintrustError::Api { status: 400, .. })
    ));
    assert!(matches!(neighbor.await, Err(WriteError::Delivery(_))));
    assert!(matches!(flush.await, Err(FlushError::Delivery(failure)) if failure.failed==2));
    assert_eq!(client.flush().await.unwrap().delivered, 0);
    let fresh =
        client.update_span_with_credentials("token", "org", &exported("fresh"), SpanLog::default());
    let flush = client.flush();
    fresh.await.unwrap();
    assert_eq!(flush.await.unwrap().delivered, 1);
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn validation_and_queue_overload_report_issuance_failure_without_network_work() {
    let server = MockServer::start().await;
    ingestion(&server).await;
    let client = client(&server, 1).await;
    let invalid =
        client.update_span_with_credentials("token", "org", "invalid", SpanLog::default());
    assert!(matches!(&*invalid, Err(BraintrustError::InvalidConfig(_))));
    let missing_login = client.update_span(&exported("row"), SpanLog::default());
    assert!(matches!(
        &*missing_login,
        Err(BraintrustError::InvalidConfig(_))
    ));
    let accepted = client.update_span_with_credentials(
        "token",
        "org",
        &exported("accepted"),
        SpanLog::default(),
    );
    let full = client.update_span_with_credentials(
        "token",
        "org",
        &exported("rejected"),
        SpanLog::default(),
    );
    assert!(accepted.is_ok());
    assert!(matches!(
        &*full,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    assert!(server.received_requests().await.unwrap().is_empty());
    client.flush().await.unwrap();
    accepted.await.unwrap();
    assert!(matches!(
        full.await,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    client.shutdown(Duration::from_secs(5)).await.unwrap();
    let closed = client.update_span_with_credentials(
        "token",
        "org",
        &exported("closed"),
        SpanLog::default(),
    );
    assert!(matches!(
        &*closed,
        Err(BraintrustError::Admission(EnqueueError::Closed))
    ));
    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(
        &requests
            .iter()
            .find(|r| r.url.path() == "/logs3")
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(body["rows"].as_array().unwrap().len(), 1);
    assert_eq!(body["rows"][0]["id"], "accepted");
}

#[tokio::test]
async fn full_response_budget_never_admits_an_extra_update() {
    let server = MockServer::start().await;
    ingestion(&server).await;
    let client = client(&server, 2048).await;
    for index in 0..1024 {
        let update = client.update_span_with_credentials(
            "token",
            "org",
            &exported(&format!("row-{index}")),
            SpanLog::default(),
        );
        assert!(update.is_ok());
        drop(update);
    }
    let rejected = client.update_span_with_credentials(
        "token",
        "org",
        &exported("rejected"),
        SpanLog::default(),
    );
    assert!(matches!(
        &*rejected,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    assert_eq!(
        client
            .shutdown(Duration::from_secs(10))
            .await
            .unwrap()
            .delivered,
        1024
    );
    let rows: usize = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/logs3")
        .map(|r| {
            serde_json::from_slice::<Value>(&r.body).unwrap()["rows"]
                .as_array()
                .unwrap()
                .len()
        })
        .sum();
    assert_eq!(rows, 1024);
}

#[tokio::test]
async fn shutdown_timeout_fails_updates_still_waiting_for_login_without_ingestion() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(60)))
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .runtime(braintrust_sdk_rust::current_runtime())
        .api_key("session")
        .blocking_login(false)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap();
    let update = client.update_span(&exported("pending"), SpanLog::default());
    assert!(update.is_ok());
    let flush = client.flush();
    assert!(matches!(
        client.shutdown(Duration::from_millis(20)).await,
        Err(ShutdownError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        update.await,
        Err(BraintrustError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        flush.await,
        Err(FlushError::Execution(ExecutionError::TimedOut))
    ));
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| r.url.path() != "/logs3"));
}

#[test]
fn stopped_runtime_retires_a_pending_login_update_that_owns_the_client() {
    let (send, receive) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = runtime
            .block_on(
                BraintrustClient::builder()
                    .runtime(Some(runtime.handle().clone()))
                    .api_key("session")
                    .blocking_login(false)
                    .app_url("http://127.0.0.1:9")
                    .api_url("http://127.0.0.1:9")
                    .build()
                    .into_future(),
            )
            .unwrap();
        let update = client.update_span(&exported("pending"), SpanLog::default());
        assert!(update.is_ok());
        drop(client);
        // Pending scope resolution retains the whole client, including its
        // lifecycle coordinator. Cancellation must release it without locking
        // the delivery state again from inside command destruction.
        drop(runtime);
        send.send(update).unwrap();
    });
    let update = receive
        .recv_timeout(Duration::from_secs(5))
        .expect("runtime loss must not deadlock while dropping an unresolved update");
    assert!(matches!(
        futures::executor::block_on(update.into_future()),
        Err(BraintrustError::Execution(ExecutionError::WorkerLost))
    ));
    thread.join().unwrap();
}
