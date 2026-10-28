use braintrust_sdk_rust::{eval::*, BraintrustClient, BraintrustError, Dataset, EnqueueError};
use serde::Deserialize;
use serde_json::json;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

async fn setup(recording: bool) -> (BraintrustClient, Dataset, MockServer, MockServer) {
    let control = MockServer::start().await;
    let data = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"org_info":[{"id":"record-org","name":"record-org","api_url":data.uri()}]}),
        ))
        .mount(&control)
        .await;
    Mock::given(method("POST")).and(path("/api/dataset/register"))
        .and(header("authorization", "Bearer dataset-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"project":{"id":"project","name":"project"},"dataset":{"id":"dataset-id","name":"cases"}})))
        .mount(&control).await;
    Mock::given(method("POST")).and(path("/api/experiment/register"))
        .and(header("authorization", "Bearer record-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"project":{"id":"record-project","name":"record-project"},"experiment":{"id":"experiment","name":"experiment"}})))
        .mount(&control).await;
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&data)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&data)
        .await;
    let client = BraintrustClient::builder()
        .runtime(braintrust_sdk_rust::current_runtime())
        .app_url(control.uri())
        .api_url(data.uri())
        .api_key("record-token")
        .skip_login(!recording)
        .blocking_login(recording)
        .default_project("record-project")
        .build()
        .await
        .unwrap();
    let dataset = client
        .dataset_builder_with_credentials("dataset-token", "dataset-org")
        .project_name("project")
        .dataset_name("cases")
        .build()
        .unwrap();
    (client, dataset, control, data)
}
fn page(response: ResponseTemplate) -> Mock {
    Mock::given(method("POST"))
        .and(path("/btql"))
        .and(header("authorization", "Bearer dataset-token"))
        .respond_with(response)
}
#[derive(Deserialize)]
struct Input {
    value: usize,
}
#[derive(Deserialize)]
struct Output {
    value: usize,
}
fn options(source: DatasetCaseSource<Input, Output>) -> EvalOpts<Input, Output> {
    EvalOpts::builder()
        .name("dataset".into())
        .source(Box::new(source))
        .definition(
            EvalDefinition::new(
                Box::new(SimpleFnTask::new(|input: &Input| {
                    Ok(Output {
                        value: input.value * 2,
                    })
                })),
                vec![],
            )
            .unwrap(),
        )
        .quiet(true)
        .build()
}

#[tokio::test]
async fn typed_pages_pin_version_and_preserve_case_fields_without_clone_or_serialize() {
    let (client, dataset, control, data) = setup(false).await;
    page(ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"one","input":{"value":1},"expected":{"value":2},"metadata":{"source":true},"tags":["source"],"_xact_id":"row-version","created":"created"}],"cursor":"a"})))
        .with_priority(1).up_to_n_times(1).expect(1).mount(&data).await;
    page(ResponseTemplate::new(200).set_body_json(json!({"data":[],"cursor":"b"})))
        .with_priority(2)
        .up_to_n_times(1)
        .expect(1)
        .mount(&data)
        .await;
    page(
        ResponseTemplate::new(200)
            .set_body_json(json!({"data":[{"id":"two","input":{"value":2}}],"cursor":null})),
    )
    .with_priority(3)
    .expect(1)
    .mount(&data)
    .await;
    let source = DatasetCaseSource::new(dataset, Some("snapshot".into()), Some(1)).unwrap();
    let summary = run(EvalRequest::local(options(source))).await.unwrap();
    assert_eq!(
        summary.dataset.unwrap(),
        DatasetVersionRef {
            dataset_id: "dataset-id".into(),
            version: Some("snapshot".into())
        }
    );
    assert!(summary.recording.is_none());
    assert_eq!(
        summary
            .results
            .iter()
            .map(|case| case.output.as_ref().unwrap().value)
            .collect::<Vec<_>>(),
        [2, 4]
    );
    let case = &summary.results[0];
    assert_eq!(case.expected.as_ref().unwrap().value, 2);
    assert_eq!(case.case_id.as_deref(), Some("one"));
    assert_eq!(case.xact_id.as_deref(), Some("row-version"));
    assert_eq!(case.created.as_deref(), Some("created"));
    assert_eq!(case.metadata.as_ref().unwrap()["source"], true);
    assert_eq!(case.tags.as_ref().unwrap(), &["source"]);
    let queries: Vec<serde_json::Value> = data
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/btql")
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(queries.len(), 3);
    for query in &queries {
        assert_eq!(query["version"], "snapshot");
        assert_eq!(query["query"]["from"], "dataset('dataset-id')");
        assert_eq!(query["query"]["limit"], 1);
    }
    assert!(queries[0]["query"].get("cursor").is_none());
    assert_eq!(queries[1]["query"]["cursor"], "a");
    assert_eq!(queries[2]["query"]["cursor"], "b");
    assert!(!control
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|request| request.url.path() == "/btql"));
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn recorded_source_admitted_before_shutdown_can_fetch_all_pages_and_finish_recording() {
    let (client, dataset, _control, data) = setup(true).await;
    page(
        ResponseTemplate::new(200)
            .set_body_json(json!({"data":[{"id":"one","input":1}],"cursor":"next"})),
    )
    .with_priority(1)
    .up_to_n_times(1)
    .mount(&data)
    .await;
    page(
        ResponseTemplate::new(200)
            .set_body_json(json!({"data":[{"id":"two","input":2}],"cursor":null})),
    )
    .with_priority(2)
    .mount(&data)
    .await;
    let source = DatasetCaseSource::<usize, usize>::new(dataset, None, Some(1)).unwrap();
    let opts = EvalOpts::builder()
        .name("recorded-dataset".into())
        .source(Box::new(source))
        .definition(
            EvalDefinition::new(
                Box::new(SimpleFnTask::new(|input: &usize| Ok(*input))),
                vec![],
            )
            .unwrap(),
        )
        .quiet(true)
        .build();
    let token = run(EvalRequest::recorded(opts, client.clone()));
    assert!(token.is_ok());
    let shutdown = client.shutdown(Duration::from_secs(5));
    let (result, shutdown) = tokio::join!(token, shutdown);
    let summary = result.unwrap();
    shutdown.unwrap();
    assert_eq!(summary.total_cases, 2);
    assert_eq!(summary.dataset.unwrap().version, None);
    assert_eq!(summary.recording.unwrap().experiment_id, "experiment");
    assert!(summary.results.iter().all(|case| case.span_id.is_some()));
}

#[tokio::test]
async fn registration_failure_is_a_typed_source_error_before_cases_are_consumed() {
    let (client, dataset, control, data) = setup(false).await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(ResponseTemplate::new(403).set_body_string("denied"))
        .with_priority(1)
        .mount(&control)
        .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let source = DatasetCaseSource::<usize, usize>::new(dataset, None, None).unwrap();
    let opts = EvalOpts::builder()
        .name("failure".into())
        .source(Box::new(source))
        .definition(
            EvalDefinition::new(
                Box::new(SimpleFnTask::new({
                    let calls = calls.clone();
                    move |input: &usize| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(*input)
                    }
                })),
                vec![],
            )
            .unwrap(),
        )
        .quiet(true)
        .build();
    let error = run(EvalRequest::local(opts)).await.unwrap_err();
    let [EvalRunFailure::Source {
        case_index: 0,
        source,
    }] = error.failures.as_slice()
    else {
        panic!("expected source failure")
    };
    assert!(source.chain().any(|error| matches!(
        error.downcast_ref::<BraintrustError>(),
        Some(BraintrustError::Api { status: 403, .. })
    )));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let partial = error.partial.unwrap();
    assert_eq!(partial.total_cases, 0);
    assert!(partial.dataset.is_none());
    // Client startup may discover ingestion capabilities; no dataset reads
    // may run outside the source's fixed execution binding.
    assert!(data
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|request| request.url.path() == "/version"));
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn later_page_failure_retains_completed_cases_and_snapshot_identity() {
    let (client, dataset, _control, data) = setup(false).await;
    page(
        ResponseTemplate::new(200)
            .set_body_json(json!({"data":[{"id":"one","input":{"value":4}}],"cursor":"next"})),
    )
    .with_priority(1)
    .up_to_n_times(1)
    .mount(&data)
    .await;
    page(ResponseTemplate::new(502).set_body_string("page failed"))
        .with_priority(2)
        .mount(&data)
        .await;
    let error = run(EvalRequest::local(options(
        DatasetCaseSource::new(dataset, Some("pinned".into()), Some(1)).unwrap(),
    )))
    .await
    .err()
    .expect("page should fail");
    assert!(matches!(
        &error.failures[..],
        [EvalRunFailure::Source { case_index: 1, .. }]
    ));
    let summary = error.partial.unwrap();
    assert_eq!(summary.dataset.unwrap().version.as_deref(), Some("pinned"));
    assert_eq!(summary.results[0].output.as_ref().unwrap().value, 8);
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn malformed_row_keeps_decode_cause_and_preceding_results() {
    let (client, dataset, _control, data) = setup(false).await;
    page(ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"good","input":{"value":2}},{"id":"bad","input":"invalid input"}],"cursor":null}))).mount(&data).await;
    let error = run(EvalRequest::local(options(
        DatasetCaseSource::new(dataset, None, None).unwrap(),
    )))
    .await
    .err()
    .expect("decode should fail");
    let [EvalRunFailure::Source {
        case_index: 1,
        source,
    }] = error.failures.as_slice()
    else {
        panic!("expected row failure")
    };
    assert!(source
        .chain()
        .any(|error| error.downcast_ref::<serde_json::Error>().is_some()));
    assert!(source.to_string().contains("bad input"));
    assert_eq!(
        error.partial.unwrap().results[0]
            .output
            .as_ref()
            .unwrap()
            .value,
        4
    );
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn dropping_an_unread_source_releases_accepted_work_and_closed_admission_is_immediate() {
    let (client, dataset, _control, data) = setup(false).await;
    let source = DatasetCaseSource::<usize, usize>::new(dataset.clone(), None, None).unwrap();
    drop(source);
    client.shutdown(Duration::from_secs(5)).await.unwrap();
    // Client startup may discover ingestion capabilities; no dataset reads
    // may run outside the source's fixed execution binding.
    assert!(data
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|request| request.url.path() == "/version"));
    let error = DatasetCaseSource::<usize, usize>::new(dataset, None, None)
        .err()
        .expect("source admission must be closed");
    assert!(error.chain().any(|error| matches!(
        error.downcast_ref::<BraintrustError>(),
        Some(BraintrustError::Admission(EnqueueError::Closed))
    )));
}

#[tokio::test]
async fn empty_live_source_reports_identity_without_claiming_a_snapshot() {
    let (client, dataset, _control, data) = setup(false).await;
    page(ResponseTemplate::new(200).set_body_json(json!({"data":[],"cursor":null})))
        .expect(1)
        .mount(&data)
        .await;
    let summary = run(EvalRequest::local(options(
        DatasetCaseSource::new(dataset, None, None).unwrap(),
    )))
    .await
    .unwrap();
    assert_eq!(summary.total_cases, 0);
    assert_eq!(
        summary.dataset.unwrap(),
        DatasetVersionRef {
            dataset_id: "dataset-id".into(),
            version: None
        }
    );
    let requests = data.received_requests().await.unwrap();
    let query: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(query.get("version").is_none());
    client.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn source_runtime_loss_reports_original_worker_loss_without_running_reads_elsewhere() {
    let control = MockServer::start().await;
    let data = MockServer::start().await;
    let app_url = control.uri();
    let api_url = data.uri();
    let source = std::thread::spawn(move || {
        use std::future::IntoFuture;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = runtime
            .block_on(
                BraintrustClient::builder()
                    .runtime(Some(runtime.handle().clone()))
                    .app_url(app_url)
                    .api_url(api_url)
                    .skip_login(true)
                    .build()
                    .into_future(),
            )
            .unwrap();
        let dataset = client
            .dataset_builder_with_credentials("token", "org")
            .project_name("project")
            .dataset_name("dataset")
            .build()
            .unwrap();
        let source = DatasetCaseSource::new(dataset, None, None).unwrap();
        drop(runtime);
        source
    })
    .join()
    .unwrap();
    let error = run(EvalRequest::local(options(source)))
        .await
        .err()
        .expect("source runtime was lost");
    let [EvalRunFailure::Source {
        case_index: 0,
        source,
    }] = error.failures.as_slice()
    else {
        panic!("expected source failure")
    };
    assert!(source.chain().any(|error| matches!(
        error.downcast_ref::<BraintrustError>(),
        Some(BraintrustError::Execution(
            braintrust_sdk_rust::ExecutionError::WorkerLost
        ))
    )));
    assert!(control.received_requests().await.unwrap().is_empty());
    // Client startup may discover ingestion capabilities; no dataset reads
    // may run outside the source's fixed execution binding.
    assert!(data
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|request| request.url.path() == "/version"));
}

#[tokio::test]
async fn recorded_dataset_selection_and_row_origins_preserve_distinct_versions() {
    for version in [Some("snapshot".to_owned()), None] {
        let (client, dataset, control, data) = setup(true).await;
        page(ResponseTemplate::new(200).set_body_json(json!({"data":[
            {"id":"one","input":1,"_xact_id":"row-one","created":"timestamp"},
            {"id":"two","input":2,"_xact_id":"row-two"},
            {"id":"three","input":3},
            {"id":"four","input":4,"_xact_id":""},
            {"id":"","input":5,"_xact_id":"row-five"}
        ],"cursor":null})))
        .expect(1)
        .mount(&data)
        .await;
        let source =
            DatasetCaseSource::<usize, usize>::new(dataset, version.clone(), None).unwrap();
        let definition = EvalDefinition::new(
            Box::new(task(|input: &usize, hooks| {
                Box::pin(async move {
                    let child = hooks.span.unwrap().child_builder()?.name("child").build();
                    child.end().await?;
                    Ok(*input)
                })
            })),
            vec![],
        )
        .unwrap();
        let opts = EvalOpts::builder()
            .name("dataset-origins".into())
            .source(Box::new(source))
            .definition(definition)
            .quiet(true)
            .build();
        let summary = run(EvalRequest::recorded(opts, client.clone()))
            .await
            .unwrap();
        client.shutdown(Duration::from_secs(5)).await.unwrap();
        assert_eq!(summary.dataset.as_ref().unwrap().version, version);
        let requests = control.received_requests().await.unwrap();
        let dataset_index = requests
            .iter()
            .position(|r| r.url.path() == "/api/dataset/register")
            .unwrap();
        let experiment_index = requests
            .iter()
            .position(|r| r.url.path() == "/api/experiment/register")
            .unwrap();
        assert!(dataset_index < experiment_index);
        let registration: serde_json::Value =
            serde_json::from_slice(&requests[experiment_index].body).unwrap();
        assert_eq!(registration["dataset_id"], "dataset-id");
        assert_eq!(
            registration.get("dataset_version"),
            version.as_ref().map(|v| json!(v)).as_ref()
        );
        let requests = data.received_requests().await.unwrap();
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
        assert!(!rows.is_empty());
        for (index, result) in summary.results.iter().enumerate() {
            let case_rows: Vec<_> = rows
                .iter()
                .filter(|r| r["span_id"] == *result.span_id.as_ref().unwrap())
                .collect();
            assert!(!case_rows.is_empty());
            for row in case_rows {
                assert_eq!(row["experiment_id"], "experiment");
                // Instrumentation context and dataset provenance are separate fields.
                assert_eq!(row["context"]["span_origin"]["name"], "braintrust.sdk.rust");
                let expected = match index {
                    0 => Some(
                        json!({"object_type":"dataset","object_id":"dataset-id","id":"one","_xact_id":"row-one","created":"timestamp"}),
                    ),
                    1 => Some(
                        json!({"object_type":"dataset","object_id":"dataset-id","id":"two","_xact_id":"row-two"}),
                    ),
                    _ => None,
                };
                assert_eq!(row.get("origin"), expected.as_ref());
            }
        }
        let children: Vec<_> = rows
            .iter()
            .filter(|r| r["span_attributes"]["name"] == "child")
            .collect();
        assert_eq!(children.len(), 5);
        assert!(children.iter().all(|r| r.get("origin").is_none()));
    }
}

#[tokio::test]
async fn failed_dataset_preparation_creates_no_experiment_and_returns_empty_partial() {
    let (client, dataset, control, data) = setup(true).await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(ResponseTemplate::new(403).set_body_string("no dataset access"))
        .with_priority(1)
        .mount(&control)
        .await;
    let source = DatasetCaseSource::<usize, usize>::new(dataset, None, None).unwrap();
    let opts = EvalOpts::builder()
        .name("failed-source".into())
        .source(Box::new(source))
        .definition(
            EvalDefinition::new(
                Box::new(SimpleFnTask::new(|_: &usize| -> anyhow::Result<usize> {
                    panic!("must not execute")
                })),
                vec![],
            )
            .unwrap(),
        )
        .quiet(true)
        .build();
    let error = run(EvalRequest::recorded(opts, client.clone()))
        .await
        .unwrap_err();
    assert!(matches!(
        error.failures.as_slice(),
        [EvalRunFailure::Source { case_index: 0, .. }]
    ));
    let partial = error.partial.unwrap();
    assert_eq!(partial.total_cases, 0);
    assert!(partial.recording.is_none());
    assert!(partial.dataset.is_none());
    client.shutdown(Duration::from_secs(5)).await.unwrap();
    assert!(!control
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|r| r.url.path() == "/api/experiment/register"));
    assert!(!data
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|r| matches!(r.url.path(), "/btql" | "/logs3")));
}

#[tokio::test]
async fn failed_experiment_registration_retains_prepared_dataset_without_reading_pages() {
    let (client, dataset, control, data) = setup(true).await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(ResponseTemplate::new(403).set_body_string("no experiment access"))
        .with_priority(1)
        .mount(&control)
        .await;
    let source =
        DatasetCaseSource::<usize, usize>::new(dataset, Some("snapshot".into()), None).unwrap();
    let opts = EvalOpts::builder()
        .name("failed-target".into())
        .source(Box::new(source))
        .definition(
            EvalDefinition::new(
                Box::new(SimpleFnTask::new(|_: &usize| -> anyhow::Result<usize> {
                    panic!("must not execute")
                })),
                vec![],
            )
            .unwrap(),
        )
        .quiet(true)
        .build();
    let error = run(EvalRequest::recorded(opts, client.clone()))
        .await
        .unwrap_err();
    assert!(matches!(
        error.failures.as_slice(),
        [EvalRunFailure::Recording {
            case_index: None,
            ..
        }]
    ));
    let partial = error.partial.unwrap();
    assert_eq!(partial.total_cases, 0);
    assert!(partial.recording.is_none());
    assert_eq!(
        partial.dataset,
        Some(DatasetVersionRef {
            dataset_id: "dataset-id".into(),
            version: Some("snapshot".into())
        })
    );
    client.shutdown(Duration::from_secs(5)).await.unwrap();
    assert!(!data
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|r| matches!(r.url.path(), "/btql" | "/logs3")));
}
