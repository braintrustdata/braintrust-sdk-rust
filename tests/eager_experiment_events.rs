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
    let feedback = experiment.log_feedback(
        "row",
        braintrust_sdk_rust::Feedback::builder().build().unwrap(),
    );
    let update = experiment.update_span("row", braintrust_sdk_rust::SpanLog::default());
    for patch in [feedback, update] {
        assert!(matches!(
            &*patch,
            Err(BraintrustError::Admission(EnqueueError::Full))
        ));
        assert!(matches!(
            patch.await,
            Err(BraintrustError::Admission(EnqueueError::Full))
        ));
    }

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
    let feedback = experiment.log_feedback(
        "row",
        braintrust_sdk_rust::Feedback::builder().build().unwrap(),
    );
    let update = experiment.update_span("row", braintrust_sdk_rust::SpanLog::default());
    for patch in [feedback, update] {
        assert!(matches!(
            &*patch,
            Err(BraintrustError::Admission(EnqueueError::Full))
        ));
        assert!(matches!(
            patch.await,
            Err(BraintrustError::Admission(EnqueueError::Full))
        ));
    }

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
    let feedback = experiment.log_feedback(
        "feedback",
        braintrust_sdk_rust::Feedback::builder().build().unwrap(),
    );
    let update = experiment.update_span("update", braintrust_sdk_rust::SpanLog::default());
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
    for patch in [feedback, update] {
        assert!(matches!(
            patch.await,
            Err(BraintrustError::Execution(ExecutionError::TimedOut))
        ));
    }
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

#[tokio::test]
async fn unobserved_patches_own_their_ids_and_share_registration_across_shutdown() {
    use braintrust_sdk_rust::{Feedback, SpanLog};
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
    let feedback = experiment.log_feedback(
        &String::from("feedback"),
        Feedback::builder()
            .expected(json!("correct"))
            .score("quality", 1.0)
            .metadata_entry("source_detail", "test")
            .comment("reviewed")
            .source("external")
            .tag("checked")
            .build()
            .unwrap(),
    );
    let update = experiment.clone().update_span(
        &String::from("update"),
        SpanLog::builder()
            .input(json!("input"))
            .output(json!("output"))
            .expected(json!("expected"))
            .error(json!("error"))
            .score("quality", 0.5)
            .metadata_entry("extra", "value")
            .metric("tokens", 12.0)
            .tag("patched")
            .name("updated span")
            .build()
            .unwrap(),
    );
    assert!(feedback.is_ok());
    assert!(update.is_ok());
    drop(feedback);
    drop(update);
    let flush = experiment.flush();
    let shutdown = client.shutdown(Duration::from_secs(5));
    let closed = experiment.log_feedback("after", Feedback::builder().build().unwrap());
    assert!(matches!(
        &*closed,
        Err(BraintrustError::Admission(EnqueueError::Closed))
    ));
    assert!(matches!(
        &*experiment.update_span("after", SpanLog::default()),
        Err(BraintrustError::Admission(EnqueueError::Closed))
    ));
    drop(experiment);
    drop(client);
    assert_eq!(flush.await.unwrap().delivered, 2);
    assert_eq!(shutdown.await.unwrap().delivered, 2);
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
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row["experiment_id"], "experiment");
        assert_eq!(row["_is_merge"], true);
    }
    let feedback = rows.iter().find(|row| row["id"] == "feedback").unwrap();
    assert_eq!(feedback["expected"], "correct");
    assert_eq!(feedback["scores"]["quality"], 1.0);
    assert_eq!(
        feedback["metadata"],
        json!({"source_detail":"test","comment":"reviewed","source":"external"})
    );
    assert_eq!(feedback["tags"], json!(["checked"]));
    let update = rows.iter().find(|row| row["id"] == "update").unwrap();
    assert_eq!(update["input"], "input");
    assert_eq!(update["output"], "output");
    assert_eq!(update["expected"], "expected");
    assert_eq!(update["error"], "error");
    assert_eq!(update["scores"]["quality"], 0.5);
    assert_eq!(update["metadata"]["extra"], "value");
    assert_eq!(update["metrics"]["tokens"], 12.0);
    assert_eq!(update["tags"], json!(["patched"]));
    assert_eq!(update["span_attributes"]["name"], "updated span");
}

#[tokio::test]
async fn patch_registration_failure_retires_the_batch_and_allows_fresh_retry() {
    use braintrust_sdk_rust::{Feedback, SpanLog};
    let server = MockServer::start().await;
    ingestion(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(ResponseTemplate::new(400))
        .with_priority(1)
        .up_to_n_times(2)
        .expect(2)
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
    let feedback = experiment.log_feedback("row", Feedback::builder().build().unwrap());
    let update = experiment.update_span("row", SpanLog::default());
    let flush = experiment.flush();
    for patch in [feedback, update] {
        assert!(patch.is_ok());
        assert!(matches!(
            patch.await,
            Err(BraintrustError::Api { status: 400, .. })
        ));
    }
    assert!(matches!(flush.await, Err(FlushError::Delivery(failure)) if failure.failed==2));
    assert_eq!(experiment.flush().await.unwrap().delivered, 0);
    assert_eq!(experiment.experiment_id(), None);
    let retry = experiment.update_span(
        "row",
        SpanLog::builder().output(json!("retry")).build().unwrap(),
    );
    let fresh = experiment.flush();
    retry.await.unwrap();
    assert_eq!(fresh.await.unwrap().delivered, 1);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests.iter().filter(|r| r.url.path() == "/logs3").count(),
        1
    );
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn explicit_patches_remain_independent_of_a_finished_live_span() {
    use braintrust_sdk_rust::{Feedback, SpanLog};
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
    let span = experiment.start_span().await.unwrap().build();
    let end = span.end();
    let update = experiment.update_span(
        span.row_id(),
        SpanLog::builder()
            .output(json!("after finish"))
            .build()
            .unwrap(),
    );
    let feedback = experiment.log_feedback(
        span.row_id(),
        Feedback::builder().score("review", 1.0).build().unwrap(),
    );
    let flush = experiment.flush();
    assert!(update.is_ok());
    assert!(feedback.is_ok());
    end.await.unwrap();
    update.await.unwrap();
    feedback.await.unwrap();
    assert_eq!(flush.await.unwrap().delivered, 3);
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(
        &requests
            .iter()
            .find(|r| r.url.path() == "/logs3")
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(body["rows"].as_array().unwrap().len(), 1);
    let row = &body["rows"][0];
    assert_eq!(row["id"], span.row_id());
    assert_eq!(row["output"], "after finish");
    assert_eq!(row["scores"]["review"], 1.0);
    assert!(row["metrics"]["end"].is_number());
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}
