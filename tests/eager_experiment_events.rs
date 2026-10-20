use braintrust_sdk_rust::{
    BraintrustClient, BraintrustError, EnqueueError, ExecutionError, Experiment, ExperimentLog,
    FlushError, ParentSpanInfo, ShutdownError, WriteError,
};
use serde_json::json;
use std::{future::IntoFuture, sync::Arc, time::Duration};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn experiment(client: &BraintrustClient) -> Experiment {
    client
        .experiment_builder_with_credentials("token", "org")
        .project_name("project")
        .build()
        .unwrap()
}
fn registration() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "project":{"id":"project","name":"project"},
        "experiment":{"id":"experiment","name":"experiment"}
    }))
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
async fn client(server: &MockServer, queue_size: usize) -> BraintrustClient {
    BraintrustClient::builder()
        .runtime(braintrust_sdk_rust::current_runtime())
        .skip_login(true)
        .app_url(server.uri())
        .api_url(server.uri())
        .with_queue_max_size(queue_size)
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn plain_thread_issuance_drains_unobserved_registration_and_delivery_after_drop() {
    let server = MockServer::start().await;
    ingestion(&server).await;
    let arrived = Arc::new(tokio::sync::Notify::new());
    let observed = arrived.clone();
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(move |_: &wiremock::Request| {
            observed.notify_one();
            registration()
        })
        .expect(1)
        .mount(&server)
        .await;
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
        let experiment = experiment(&client);
        let event = experiment.log(ExperimentLog::builder().input(json!("accepted")).build());
        assert!(event.is_ok());
        drop(event);
        let flush = experiment.flush();
        let shutdown = client.shutdown(Duration::from_secs(5));
        drop(experiment);
        drop(client);
        (flush, shutdown)
    })
    .join()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    assert_eq!(flush.await.unwrap().delivered, 1);
    shutdown.await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let rows: Vec<serde_json::Value> = requests
        .iter()
        .filter(|r| r.url.path() == "/logs3")
        .flat_map(|r| {
            serde_json::from_slice::<serde_json::Value>(&r.body).unwrap()["rows"]
                .as_array()
                .unwrap()
                .clone()
        })
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["experiment_id"], "experiment");
    assert_eq!(rows[0]["input"], "accepted");
    assert!(rows[0]["metrics"]["start"].is_number());
    assert!(rows[0]["metrics"]["end"].is_number());
    assert_eq!(rows[0]["span_attributes"]["type"], "eval");
}

#[tokio::test]
async fn shutdown_before_first_poll_covers_registration_and_the_final_write() {
    let server = MockServer::start().await;
    ingestion(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(registration())
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server, 10).await;
    let experiment = experiment(&client);
    let event = experiment.log(ExperimentLog::default());
    let shutdown = client.shutdown(Duration::from_secs(5));
    assert!(event.is_ok());
    let rejected = experiment.log(ExperimentLog::default());
    assert!(matches!(
        &*rejected,
        Err(BraintrustError::Admission(EnqueueError::Closed))
    ));
    assert_eq!(shutdown.await.unwrap().delivered, 1);
    let id = event.await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(
        &requests
            .iter()
            .find(|r| r.url.path() == "/logs3")
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(body["rows"][0]["id"], id);
}

#[tokio::test]
async fn registration_failure_fails_the_batch_then_retires_before_a_fresh_event() {
    let server = MockServer::start().await;
    ingestion(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(registration())
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server, 10).await;
    let experiment = experiment(&client);
    let event = experiment.log(ExperimentLog::default());
    let neighbor = client
        .span_builder_with_credentials("token", "org")
        .parent_info(ParentSpanInfo::ProjectLogs {
            object_id: "project".into(),
        })
        .build()
        .end();
    let flush = experiment.flush();
    assert!(matches!(
        event.await,
        Err(BraintrustError::Api { status: 503, .. })
    ));
    assert!(matches!(neighbor.await, Err(WriteError::Delivery(_))));
    assert!(matches!(flush.await, Err(FlushError::Delivery(failure)) if failure.failed==2));
    assert_eq!(experiment.flush().await.unwrap().delivered, 0);
    let next = experiment.clone().log(ExperimentLog::default());
    let fresh = experiment.flush();
    assert!(next.is_ok());
    assert!(!next.await.unwrap().is_empty());
    assert_eq!(fresh.await.unwrap().delivered, 1);
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn queue_rejection_never_registers_and_the_reserved_response_slot_is_retired() {
    let server = MockServer::start().await;
    ingestion(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(registration())
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server, 1).await;
    let experiment = experiment(&client);
    let existing = client
        .span_builder_with_credentials("token", "org")
        .parent_info(ParentSpanInfo::ProjectLogs {
            object_id: "project".into(),
        })
        .build()
        .end();
    let rejected = experiment.log(ExperimentLog::default());
    assert!(matches!(
        &*rejected,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(matches!(
        rejected.await,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    client.flush().await.unwrap();
    existing.await.unwrap();
    let retry = experiment.log(ExperimentLog::default());
    let flush = experiment.flush();
    assert!(retry.is_ok());
    retry.await.unwrap();
    assert_eq!(flush.await.unwrap().delivered, 1);
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn full_response_admission_never_creates_an_extra_write_obligation() {
    let server = MockServer::start().await;
    ingestion(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(registration())
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server, 2048).await;
    let experiment = experiment(&client);
    for _ in 0..1024 {
        let event = experiment.log(ExperimentLog::default());
        assert!(event.is_ok());
        drop(event);
    }
    let rejected = experiment.log(ExperimentLog::default());
    assert!(matches!(
        &*rejected,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    let shutdown = client.shutdown(Duration::from_secs(10));
    assert_eq!(shutdown.await.unwrap().delivered, 1024);
    let count: usize = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/logs3")
        .map(|r| {
            serde_json::from_slice::<serde_json::Value>(&r.body).unwrap()["rows"]
                .as_array()
                .unwrap()
                .len()
        })
        .sum();
    assert_eq!(count, 1024);
}

#[tokio::test]
async fn deadline_cancels_deferred_registration_and_never_submits_its_row() {
    let server = MockServer::start().await;
    let arrived = Arc::new(tokio::sync::Notify::new());
    let observed = arrived.clone();
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(move |_: &wiremock::Request| {
            observed.notify_one();
            registration().set_delay(Duration::from_secs(60))
        })
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server, 10).await;
    let experiment = experiment(&client);
    let event = experiment.log(ExperimentLog::default());
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    let shutdown = client.shutdown(Duration::from_millis(20));
    assert!(matches!(
        shutdown.await,
        Err(ShutdownError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        event.await,
        Err(BraintrustError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        &*experiment.log(ExperimentLog::default()),
        Err(BraintrustError::Admission(EnqueueError::Closed))
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[test]
fn stopped_runtime_retires_an_unpolled_event_that_owns_the_last_queue_producer() {
    let (send, receive) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = runtime
            .block_on(
                BraintrustClient::builder()
                    .skip_login(true)
                    .runtime(Some(runtime.handle().clone()))
                    .build()
                    .into_future(),
            )
            .unwrap();
        let experiment = experiment(&client);
        let event = experiment.log(ExperimentLog::default());
        assert!(event.is_ok());
        drop(experiment);
        drop(client);
        // Deferred preparation owns the only remaining queue producer. Runtime
        // loss drops it while canceling the worker, before its first poll.
        drop(runtime);
        send.send(event).unwrap();
    });
    let event = receive
        .recv_timeout(Duration::from_secs(5))
        .expect("runtime destruction must not deadlock on deferred producer cleanup");
    assert!(matches!(
        futures::executor::block_on(event.into_future()),
        Err(BraintrustError::Execution(ExecutionError::WorkerLost))
    ));
    thread.join().unwrap();
}
