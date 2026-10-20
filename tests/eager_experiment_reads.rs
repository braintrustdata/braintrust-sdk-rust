use braintrust_sdk_rust::{
    BraintrustClient, BraintrustError, EnqueueError, ExecutionError, Experiment, ExperimentLog,
    ParentSpanInfo, ShutdownError, WriteError,
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
async fn endpoints(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(registration())
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/base_experiment/get_id"))
        .respond_with(ResponseTemplate::new(400))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/experiment-comparison2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(server)
        .await;
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

#[tokio::test]
async fn plain_thread_reads_complete_unobserved_through_shutdown_after_owner_drop() {
    let server = MockServer::start().await;
    endpoints(&server).await;
    let uri = server.uri();
    let (project, exported, base, summary, span, shutdown) = std::thread::spawn(move || {
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
        let project = experiment.project();
        let exported = experiment.export();
        let base = experiment.fetch_base_experiment();
        let summary = experiment.summarize();
        let span = experiment.start_span();
        assert!(project.is_ok());
        assert!(exported.is_ok());
        assert!(base.is_ok());
        assert!(summary.is_ok());
        assert!(span.is_ok());
        let shutdown = client.shutdown(Duration::from_secs(5));
        drop(experiment);
        drop(client);
        (project, exported, base, summary, span, shutdown)
    })
    .join()
    .unwrap();
    shutdown.await.unwrap();
    assert_eq!(project.await.unwrap().id, "project");
    assert_eq!(exported.await.unwrap().object_id, "experiment");
    assert!(base.await.unwrap().is_none());
    assert_eq!(summary.await.unwrap().experiment_id(), Some("experiment"));
    // An accepted builder request may finish during shutdown, but its returned
    // live handle does not acquire permission to submit new writes afterward.
    assert!(matches!(
        &*span.await.unwrap().build().end(),
        Err(WriteError::Admission(EnqueueError::Closed))
    ));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.url.path() == "/api/base_experiment/get_id")
            .count(),
        2
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.url.path() == "/experiment-comparison2")
            .count(),
        1
    );
    assert!(requests.iter().all(|r| r.url.path() != "/logs3"));
}

#[tokio::test]
async fn full_task_budget_allows_summary_subrequests_and_rejected_starts_do_not_select_mode() {
    let server = MockServer::start().await;
    endpoints(&server).await;
    let client = client(&server, 10).await;
    let experiment = experiment(&client);
    let projects: Vec<_> = (0..1023).map(|_| experiment.project()).collect();
    assert!(projects.iter().all(|project| project.is_ok()));
    let summary = experiment.summarize();
    assert!(summary.is_ok());
    let rejected = experiment.start_span();
    assert!(matches!(
        &*rejected,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    assert!(matches!(
        &*experiment.fetch_base_experiment(),
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    summary.await.unwrap();
    for project in projects {
        assert_eq!(project.await.unwrap().id, "project");
    }
    assert!(matches!(
        rejected.await,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    let event = experiment.log(ExperimentLog::default());
    assert!(event.is_ok()); // The rejected start left event mode available.
    let flush = experiment.flush();
    event.await.unwrap();
    assert_eq!(flush.await.unwrap().delivered, 1);
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn summary_captures_its_cutoff_before_a_later_write_fails() {
    let server = MockServer::start().await;
    endpoints(&server).await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(400))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server, 10).await;
    let experiment = experiment(&client);
    let prior = experiment.log(ExperimentLog::default());
    let summary = experiment.summarize();
    let later = experiment.log(ExperimentLog::default());
    assert!(summary.is_ok());
    prior.await.unwrap();
    assert!(matches!(
        later.await,
        Err(BraintrustError::Api { status: 400, .. })
    ));
    assert_eq!(summary.await.unwrap().experiment_id(), Some("experiment"));
    assert_eq!(experiment.flush().await.unwrap().delivered, 0);
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn pending_span_starts_block_events_and_failed_registration_releases_mode() {
    let server = MockServer::start().await;
    endpoints(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(ResponseTemplate::new(400))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server, 10).await;
    let experiment = experiment(&client);
    let start = experiment.start_span();
    assert!(start.is_ok());
    let blocked = experiment.clone().log(ExperimentLog::default());
    assert!(matches!(&*blocked, Err(BraintrustError::InvalidConfig(_))));
    assert!(matches!(
        start.await,
        Err(BraintrustError::Api { status: 400, .. })
    ));
    let event = experiment.log(ExperimentLog::default());
    assert!(event.is_ok());
    event.await.unwrap();
    let accepted = experiment.start_span();
    assert!(matches!(
        &*experiment.log(ExperimentLog::default()),
        Err(BraintrustError::InvalidConfig(_))
    ));
    drop(accepted.await.unwrap());
    assert!(matches!(
        &*experiment.clone().log(ExperimentLog::default()),
        Err(BraintrustError::InvalidConfig(_))
    ));
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn deadline_resolves_all_accepted_read_and_builder_tokens_without_nested_admission() {
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
    let project = experiment.project();
    let exported = experiment.export();
    let base = experiment.fetch_base_experiment();
    let summary = experiment.summarize();
    let span = experiment.start_span();
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    assert!(matches!(
        client.shutdown(Duration::from_millis(20)).await,
        Err(ShutdownError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        project.await,
        Err(BraintrustError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        exported.await,
        Err(BraintrustError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        base.await,
        Err(BraintrustError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        summary.await,
        Err(BraintrustError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        span.await,
        Err(BraintrustError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        &*experiment.project(),
        Err(BraintrustError::Admission(EnqueueError::Closed))
    ));
    assert!(matches!(
        &*experiment.export(),
        Err(BraintrustError::Admission(EnqueueError::Closed))
    ));
    assert!(matches!(
        &*experiment.fetch_base_experiment(),
        Err(BraintrustError::Admission(EnqueueError::Closed))
    ));
    assert!(matches!(
        &*experiment.summarize(),
        Err(BraintrustError::Admission(EnqueueError::Closed))
    ));
    assert!(matches!(
        &*experiment.start_span(),
        Err(BraintrustError::Admission(EnqueueError::Closed))
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn summary_flush_rejection_reports_issuance_failure_without_starting_http_work() {
    let server = MockServer::start().await;
    endpoints(&server).await;
    let client = client(&server, 1).await;
    let experiment = experiment(&client);
    let write = client
        .span_builder_with_credentials("token", "org")
        .parent_info(ParentSpanInfo::ProjectLogs {
            object_id: "project".into(),
        })
        .build()
        .end();
    let occupied = client.flush();
    let rejected = experiment.summarize();
    assert!(matches!(
        &*rejected,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    occupied.await.unwrap();
    write.await.unwrap();
    assert!(matches!(
        rejected.await,
        Err(BraintrustError::Admission(EnqueueError::Full))
    ));
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| r.url.path() != "/api/experiment/register"));
    experiment.summarize().await.unwrap();
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}
