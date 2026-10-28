//! Login-dependent client operations are accepted before their tokens are polled.
use braintrust_sdk_rust::{
    BraintrustClient, BraintrustError, EnqueueError, ExecutionError, ShutdownError,
};
use serde_json::json;
use std::{future::IntoFuture, time::Duration};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn is_closed<T>(completion: &braintrust_sdk_rust::Completion<T, BraintrustError>) -> bool {
    matches!(
        &**completion,
        Err(BraintrustError::Admission(EnqueueError::Closed))
    )
}

#[test]
fn skipped_login_rejects_issuance_without_polling_or_a_call_site_runtime() {
    let client = futures::executor::block_on(
        BraintrustClient::builder()
            .skip_login(true)
            .build()
            .into_future(),
    )
    .unwrap();
    assert!(!client.is_logged_in());
    assert!(!client.login_state().is_logged_in());
    let wait = client.wait_for_login();
    let span = client.span_builder();
    let dataset = client.dataset_builder();
    assert!(matches!(&*wait, Err(BraintrustError::InvalidConfig(_))));
    assert!(matches!(&*span, Err(BraintrustError::InvalidConfig(_))));
    assert!(matches!(&*dataset, Err(BraintrustError::InvalidConfig(_))));
    assert!(matches!(
        futures::executor::block_on(wait.into_future()),
        Err(BraintrustError::InvalidConfig(_))
    ));
    assert!(matches!(
        futures::executor::block_on(span.into_future()),
        Err(BraintrustError::InvalidConfig(_))
    ));
    assert!(matches!(
        futures::executor::block_on(dataset.into_future()),
        Err(BraintrustError::InvalidConfig(_))
    ));
}

#[tokio::test]
async fn synchronous_issuance_and_cross_executor_observation_share_login_and_shutdown() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "org_info":[{"id":"org","name":"tenant"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let uri = server.uri();
    let (wait, span, dataset, shutdown) = std::thread::spawn(move || {
        assert!(braintrust_sdk_rust::current_runtime().is_none());
        let client = futures::executor::block_on(
            BraintrustClient::builder()
                .api_key("token")
                .blocking_login(false)
                .app_url(&uri)
                .api_url(&uri)
                .build()
                .into_future(),
        )
        .unwrap();
        let wait = client.wait_for_login();
        let span = client.span_builder();
        let dataset = client.dataset_builder();
        assert!(wait.is_ok());
        assert!(span.is_ok());
        assert!(dataset.is_ok());
        // No observation starts these operations. Shutdown must drain the
        // background login and each accepted builder/wait before completing.
        let shutdown = client.shutdown(Duration::from_secs(5));
        drop(client);
        (wait, span, dataset, shutdown)
    })
    .join()
    .unwrap();
    shutdown.await.unwrap();
    assert_eq!(wait.await.unwrap().org_id().as_deref(), Some("org"));
    let _span = span.await.unwrap().name("accepted").build();
    let dataset = dataset
        .await
        .unwrap()
        .project_name("p")
        .dataset_name("d")
        .build()
        .unwrap();
    assert_eq!(dataset.dataset_name(), "d");
    // Obtaining/building the handle didn't register the dataset or write a span.
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn shutdown_deadline_resolves_unobserved_login_waits_and_resource_builder_requests() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(60)))
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .api_key("token")
        .blocking_login(false)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap();
    let wait = client.wait_for_login();
    let span = client.span_builder();
    let dataset = client.dataset_builder();
    assert!(wait.is_ok());
    assert!(span.is_ok());
    assert!(dataset.is_ok());
    assert!(matches!(
        client.shutdown(Duration::from_millis(20)).await,
        Err(ShutdownError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        wait.await,
        Err(BraintrustError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        span.await,
        Err(BraintrustError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        dataset.await,
        Err(BraintrustError::Execution(ExecutionError::TimedOut))
    ));
    assert!(is_closed(&client.wait_for_login()));
    assert!(is_closed(&client.span_builder()));
    assert!(is_closed(&client.dataset_builder()));
    // Local metadata remains available after work admission closes.
    assert!(!client.is_logged_in());
    assert!(!client.login_state().is_logged_in());
}

#[tokio::test]
async fn unobserved_client_requests_share_the_bounded_task_budget() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(60)))
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .runtime(braintrust_sdk_rust::current_runtime())
        .api_key("token")
        .blocking_login(false)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap();
    // Background login already occupies one slot. Dropped observations keep
    // their accepted waits active, so all 1,023 remaining slots are consumed.
    for _ in 0..1023 {
        let wait = client.wait_for_login();
        assert!(wait.is_ok());
        drop(wait);
    }
    let span = client.span_builder();
    let dataset = client.dataset_builder();
    assert!(matches!(
        &*span,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    assert!(matches!(
        &*dataset,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    assert!(matches!(
        span.await,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    assert!(matches!(
        dataset.await,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    assert!(matches!(
        client.shutdown(Duration::from_millis(20)).await,
        Err(ShutdownError::Execution(ExecutionError::TimedOut))
    ));
}

#[tokio::test]
async fn cached_login_does_not_move_scheduled_work_off_a_stopped_explicit_runtime() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "org_info":[{"id":"org","name":"tenant"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let uri = server.uri();
    let client = tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(
                BraintrustClient::builder()
                    .runtime(Some(runtime.handle().clone()))
                    .api_key("token")
                    .blocking_login(true)
                    .app_url(&uri)
                    .api_url(&uri)
                    .build()
                    .into_future(),
            )
            .unwrap()
        // The selected runtime is dropped before returning the client.
    })
    .await
    .unwrap();
    assert!(client.is_logged_in());
    assert_eq!(client.login_state().org_id().as_deref(), Some("org"));
    let wait = client.wait_for_login();
    let span = client.span_builder();
    let dataset = client.dataset_builder();
    assert!(matches!(
        wait.await,
        Err(BraintrustError::Execution(ExecutionError::WorkerLost))
    ));
    assert!(matches!(
        span.await,
        Err(BraintrustError::Execution(ExecutionError::WorkerLost))
    ));
    assert!(matches!(
        dataset.await,
        Err(BraintrustError::Execution(ExecutionError::WorkerLost))
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
