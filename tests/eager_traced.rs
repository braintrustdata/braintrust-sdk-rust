mod support;

use braintrust_sdk_rust::{
    BraintrustClient, BraintrustError, EnqueueError, ExecutionError, ShutdownError, SpanLog,
    WriteError,
};
use serde_json::json;
use std::{future::IntoFuture, sync::Arc, time::Duration};
use support::bound_client as client;
use support::experiment;
use tokio::sync::oneshot;
use wiremock::MockServer;

async fn endpoints(server: &MockServer) {
    support::register_experiment().expect(1).mount(server).await;
    support::version().mount(server).await;
    support::logs(204).mount(server).await;
}

#[tokio::test]
async fn shutdown_before_first_poll_includes_callback_writes_and_revokes_escaped_children() {
    let server = MockServer::start().await;
    endpoints(&server).await;
    let client = client(&server, 8).await;
    let experiment = experiment(&client);
    let traced = experiment.traced(|root| async move {
        root.log(SpanLog::builder().input(json!(1)).build())
            .await
            .unwrap();
        let child = root.child_builder().unwrap().name("child").build();
        child
            .log(SpanLog::builder().output(json!(2)).build())
            .await
            .unwrap();
        child
    });
    assert!(traced.is_ok());
    let cutoff = client.flush();
    let shutdown = client.shutdown(Duration::from_secs(5));
    assert_eq!(cutoff.await.unwrap().delivered(), 0);
    assert_eq!(shutdown.await.unwrap().delivered(), 3);
    let child = traced.await.unwrap();
    assert!(child.end_time().is_none());
    assert!(matches!(
        child.end().await,
        Err(WriteError::Admission(EnqueueError::Closed))
    ));
    let requests = server.received_requests().await.unwrap();
    let rows: Vec<serde_json::Value> = support::log_rows(&requests);
    let child_row = rows
        .iter()
        .find(|r| r["span_attributes"]["name"] == "child")
        .unwrap();
    let root_row = rows
        .iter()
        .find(|r| r["span_attributes"]["name"] == "root")
        .unwrap();
    assert_eq!(child_row["root_span_id"], root_row["root_span_id"]);
    assert_eq!(child_row["span_parents"][0], root_row["span_id"]);
    assert!(rows
        .iter()
        .any(|r| r["span_attributes"]["name"] == "root" && r["metrics"]["end"].is_number()));
}

#[tokio::test]
async fn owned_runtime_runs_dropped_callback_observers_from_a_plain_thread() {
    let server = MockServer::start().await;
    endpoints(&server).await;
    let uri = server.uri();
    let (done, observed) = oneshot::channel();
    let shutdown = std::thread::spawn(move || {
        let client =
            futures::executor::block_on(support::client_builder(&uri).build().into_future())
                .unwrap();
        let experiment = experiment(&client);
        drop(experiment.traced(move |span| async move {
            span.log(SpanLog::builder().input(json!("owned")).build())
                .await
                .unwrap();
            done.send(()).unwrap();
        }));
        client.shutdown(Duration::from_secs(5))
    })
    .join()
    .unwrap();
    assert_eq!(shutdown.await.unwrap().delivered(), 2);
    observed.await.unwrap();
}

#[tokio::test]
async fn timeout_cancels_callback_and_revokes_its_escaped_write_permission() {
    let server = MockServer::start().await;
    endpoints(&server).await;
    let client = client(&server, 4).await;
    let experiment = experiment(&client);
    let (sent, escaped) = oneshot::channel();
    let traced = experiment.traced(move |root| async move {
        assert!(sent.send(root.child_builder().unwrap().build()).is_ok());
        std::future::pending::<()>().await;
    });
    let child = escaped.await.unwrap();
    assert!(matches!(
        client.shutdown(Duration::from_millis(20)).await,
        Err(ShutdownError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        traced.await,
        Err(BraintrustError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        child.end().await,
        Err(WriteError::Admission(EnqueueError::Closed))
    ));
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| r.url.path() != "/logs3"));
}

#[tokio::test]
async fn response_overload_rejects_before_callback_and_releases_capacity_after_completion() {
    let server = MockServer::start().await;
    endpoints(&server).await;
    let client = client(&server, 2048).await;
    let experiment = experiment(&client);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let mut accepted = Vec::new();
    for _ in 0..1024 {
        let waiting = gate.clone();
        let response = experiment.traced(move |_| async move {
            waiting.acquire().await.unwrap().forget();
        });
        assert!(response.submission().is_ok());
        accepted.push(response);
    }
    let rejected = experiment.traced(|_| async { panic!("rejected callback ran") });
    assert!(matches!(
        rejected.await,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    gate.add_permits(1024);
    for response in accepted {
        response.await.unwrap();
    }
    assert_eq!(experiment.traced(|_| async { 42 }).await.unwrap(), 42);
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn response_admission_is_independent_of_write_capacity() {
    let server = MockServer::start().await;
    endpoints(&server).await;
    let client = client(&server, 1).await;
    let experiment = experiment(&client);
    let (release_first, first_gate) = oneshot::channel();
    let (release_second, second_gate) = oneshot::channel();
    let first = experiment.traced(|_| async { first_gate.await.unwrap() });
    let second = experiment.traced(|_| async { second_gate.await.unwrap() });
    assert!(first.submission().is_ok());
    assert!(second.submission().is_ok());
    release_first.send(()).unwrap();
    first.await.unwrap();
    release_second.send(()).unwrap();
    second.await.unwrap();
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn panic_releases_the_operation_and_a_later_callback_can_finish() {
    let server = MockServer::start().await;
    endpoints(&server).await;
    let client = client(&server, 1).await;
    let experiment = experiment(&client);
    assert!(matches!(
        experiment
            .traced(|_| async { panic!("callback panic") })
            .await,
        Err(BraintrustError::Execution(ExecutionError::WorkerLost))
    ));
    assert_eq!(
        experiment
            .traced(|_| async { Err::<(), _>("user error") })
            .await
            .unwrap(),
        Err("user error")
    );
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[test]
fn runtime_loss_before_first_poll_retires_callback_captures_without_deadlocking() {
    struct Capture(std::sync::mpsc::Sender<()>);
    impl Drop for Capture {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
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
                    .skip_login(true)
                    .api_url("http://127.0.0.1:9")
                    .build()
                    .into_future(),
            )
            .unwrap();
        let experiment = experiment(&client);
        let (dropped, observed) = std::sync::mpsc::channel();
        let capture = Capture(dropped);
        let traced = experiment.traced(move |_| async move {
            drop(capture);
            panic!("callback must not run on a stopped runtime");
        });
        assert!(traced.is_ok());
        drop(experiment);
        drop(client);
        drop(runtime);
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        send.send(traced).unwrap();
    });
    let traced = receive
        .recv_timeout(Duration::from_secs(5))
        .expect("dropping an unpolled callback must release its lease without deadlocking");
    assert!(matches!(
        futures::executor::block_on(traced.into_future()),
        Err(BraintrustError::Execution(ExecutionError::WorkerLost))
    ));
    thread.join().unwrap();
}
