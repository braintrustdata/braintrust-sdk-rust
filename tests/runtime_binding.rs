use braintrust_sdk_rust::{
    current_runtime, BraintrustClient, BraintrustError, EnqueueError, ExecutionError,
    ParentSpanInfo, WriteError,
};
use serde_json::json;
use std::future::IntoFuture;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn synchronous_callers_deliver_without_a_tokio_runtime_and_observe_on_another_executor() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let uri = server.uri();
    let (sender, result) = tokio::sync::oneshot::channel();
    let thread = std::thread::spawn(move || {
        assert!(current_runtime().is_none());
        let initialization = BraintrustClient::builder()
            .skip_login(true)
            .api_url(&uri)
            .app_url(&uri)
            .build();
        assert!(initialization.is_ok());
        let client = futures::executor::block_on(initialization.into_future()).unwrap();
        let span = client
            .span_builder_with_credentials("token", "org")
            .parent_info(ParentSpanInfo::ProjectLogs {
                object_id: "project".into(),
            })
            .build();
        let end = span.end();
        let shutdown = client.shutdown(std::time::Duration::from_secs(5));
        drop(span);
        drop(client);
        sender.send((end, shutdown)).unwrap();
    });
    let (end, shutdown) = result.await.unwrap();
    end.await.unwrap();
    shutdown.await.unwrap();
    thread.join().unwrap();
}

#[tokio::test]
async fn unobserved_construction_starts_login_without_polling_its_token() {
    let server = MockServer::start().await;
    let arrived = std::sync::Arc::new(tokio::sync::Notify::new());
    let observed = arrived.clone();
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(move |_: &wiremock::Request| {
            observed.notify_one();
            ResponseTemplate::new(200)
                .set_body_json(json!({"org_info":[{"id":"org","name":"tenant"}]}))
        })
        .expect(1)
        .mount(&server)
        .await;
    drop(
        BraintrustClient::builder()
            .api_key("token")
            .blocking_login(true)
            .api_url(server.uri())
            .app_url(server.uri())
            .build(),
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
}

#[test]
fn explicit_current_thread_runtime_must_be_driven_and_observation_does_not_move_work() {
    let selected = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let observer = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let initialization = BraintrustClient::builder()
        .skip_login(true)
        .runtime(Some(selected.handle().clone()))
        .build();
    assert!(initialization.is_ok());
    let mut initialization = Box::pin(initialization.into_future());
    observer.block_on(async {
        assert!(futures::poll!(initialization.as_mut()).is_pending());
    });
    selected.block_on(async {
        tokio::task::yield_now().await;
    });
    let client = observer.block_on(initialization).unwrap();
    let mut shutdown = Box::pin(
        client
            .shutdown(std::time::Duration::from_secs(1))
            .into_future(),
    );
    observer.block_on(async {
        assert!(futures::poll!(shutdown.as_mut()).is_pending());
    });
    selected.block_on(async {
        tokio::task::yield_now().await;
    });
    observer.block_on(shutdown).unwrap();
    drop(client);
    // SDK shutdown did not stop the caller-owned runtime.
    assert_eq!(selected.block_on(async { 42 }), 42);
}

#[tokio::test]
async fn stopped_explicit_runtime_fails_delivery_and_resource_requests_without_fallback() {
    let server = MockServer::start().await;
    let uri = server.uri();
    let client = std::thread::spawn(move || {
        let selected = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        selected
            .block_on(
                BraintrustClient::builder()
                    .skip_login(true)
                    .runtime(Some(selected.handle().clone()))
                    .api_url(&uri)
                    .app_url(&uri)
                    .build()
                    .into_future(),
            )
            .unwrap()
    })
    .join()
    .unwrap();
    let span = client
        .span_builder_with_credentials("token", "org")
        .parent_info(ParentSpanInfo::ProjectLogs {
            object_id: "project".into(),
        })
        .build();
    // A stopped runtime may cancel the worker before or after queue admission.
    let end = span.end().await;
    assert!(
        matches!(
            end,
            Err(WriteError::Execution(ExecutionError::WorkerLost))
                | Err(WriteError::Admission(EnqueueError::Closed))
        ),
        "unexpected end result: {end:?}"
    );
    let dataset = client
        .dataset_builder_with_credentials("token", "tenant")
        .project_name("p")
        .dataset_name("d")
        .build()
        .unwrap();
    assert!(matches!(
        dataset.dataset_id().await,
        Err(BraintrustError::Execution(ExecutionError::WorkerLost))
    ));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn invalid_construction_reports_issuance_failure_synchronously() {
    let build = BraintrustClient::builder()
        .skip_login(true)
        .api_url("invalid URL")
        .build();
    assert!(matches!(&*build, Err(BraintrustError::InvalidConfig(_))));
    assert!(matches!(
        futures::executor::block_on(build.into_future()),
        Err(BraintrustError::InvalidConfig(_))
    ));
}
