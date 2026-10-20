use braintrust_sdk_rust::{
    api::{
        self,
        datasets::{DatasetFields, DatasetWrite},
        logs3::InsertRows,
    },
    Completion, EnqueueError, ExecutionError, ShutdownError,
};
use serde_json::json;
use std::{future::IntoFuture, time::Duration};
use wiremock::{
    matchers::{body_json, method, path},
    Mock, MockServer, ResponseTemplate,
};

fn config(server: &MockServer) -> api::Config {
    let mut config = api::Config::new("token");
    config.api_url = server.uri();
    config.app_url = server.uri();
    config
}
fn abandon<T>(completion: Completion<T, api::Error>) {
    assert!(completion.is_ok());
    drop(completion);
}

#[tokio::test]
async fn all_endpoint_families_run_unobserved_from_a_plain_thread_and_drain_after_owner_drop() {
    use api::{
        btql::{BTQLQuery, BTQLQueryInner},
        datasets::DatasetRegisterRequest,
        experiments::{BaseExperimentRequest, ExperimentRegisterRequest},
        logs3::{Logs3OverflowReference, Logs3OverflowRequest, Logs3OverflowUploadRequest},
        projects::ProjectRegisterRequest,
    };
    let server = MockServer::start().await;
    let fixtures = [
        ("POST", "/api/apikey/login", json!({"org_info":[]})),
        (
            "POST",
            "/api/project/register",
            json!({"project":{"id":"p"}}),
        ),
        (
            "POST",
            "/api/dataset/register",
            json!({"project":{"id":"p","name":"p"},"dataset":{"id":"d","name":"d"}}),
        ),
        (
            "GET",
            "/dataset-summary",
            json!({"project_name":"p","dataset_name":"d","project_id":"p","dataset_id":"d"}),
        ),
        ("POST", "/btql", json!({"data":[],"cursor":null})),
        (
            "POST",
            "/api/experiment/register",
            json!({"project":{"id":"p","name":"p"},"experiment":{"id":"e","name":"e"}}),
        ),
        ("POST", "/api/base_experiment/get_id", json!({})),
        ("GET", "/experiment-comparison2", json!({})),
        ("GET", "/version", json!({})),
        (
            "POST",
            "/logs3/overflow",
            json!({"method":"PUT","signedUrl":"http://unused.invalid","key":"k"}),
        ),
    ];
    for (verb, endpoint, response) in fixtures {
        Mock::given(method(verb))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .expect(4)
        .mount(&server)
        .await;
    let config = config(&server);
    let shutdown = std::thread::spawn(move || {
        let client = api::Client::new(config).unwrap();
        abandon(client.login());
        abandon(client.projects().register(ProjectRegisterRequest {
            project_name: "p".into(),
            org_id: None,
            org_name: None,
        }));
        abandon(client.datasets().register(DatasetRegisterRequest {
            project_name: "p".into(),
            org_name: "org".into(),
            dataset_name: "d".into(),
            description: None,
            metadata: None,
        }));
        abandon(client.datasets().write(&[DatasetWrite::Delete {
            dataset_id: "d".into(),
            id: "row".into(),
        }]));
        abandon(client.datasets().summarize(&String::from("d")));
        abandon(client.btql().query_dataset_records(BTQLQuery {
            query: BTQLQueryInner {
                from: "dataset('d')".into(),
                limit: None,
                cursor: None,
            },
        }));
        abandon(client.experiments().register(ExperimentRegisterRequest {
            project_name: "p".into(),
            org_id: "org".into(),
            experiment_name: None,
            description: None,
            base_experiment: None,
            metadata: None,
            public: None,
            update: None,
            repo_info: None,
        }));
        abandon(
            client
                .experiments()
                .base(BaseExperimentRequest { id: "e".into() }),
        );
        abandon(
            client
                .experiments()
                .compare(&String::from("e"), Some(&String::from("base"))),
        );
        abandon(client.logs3().version());
        abandon(
            client
                .logs3()
                .insert(InsertRows::new(vec![json!({"id":"insert"})])),
        );
        abandon(
            client
                .logs3()
                .post_raw(br#"{"rows":[],"api_version":2}"#.to_vec()),
        );
        abandon(
            client
                .logs3()
                .request_overflow_upload(Logs3OverflowUploadRequest {
                    content_type: "application/json".into(),
                    size_bytes: 2,
                    rows: vec![],
                }),
        );
        abandon(client.logs3().commit_overflow(Logs3OverflowRequest {
            rows: Logs3OverflowReference {
                reference_type: "file",
                key: "k".into(),
            },
            api_version: 2,
        }));
        client.shutdown(Duration::from_secs(5))
    })
    .join()
    .unwrap();
    shutdown.await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 14);
}

#[tokio::test]
async fn borrowed_request_data_is_captured_at_issuance_and_shutdown_closes_all_views() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(body_json(
            json!({"rows":[{"dataset_id":"d","id":"original","input":1}],"api_version":2}),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let mut config = config(&server);
    config.runtime = braintrust_sdk_rust::current_runtime();
    let client = api::Client::new(config).unwrap();
    let view = client.datasets();
    let mut rows = vec![DatasetWrite::Insert {
        dataset_id: "d".into(),
        id: "original".into(),
        fields: DatasetFields {
            input: Some(json!(1)),
            ..Default::default()
        },
    }];
    let write = view.write(&rows);
    rows.clear();
    let shutdown = client.shutdown(Duration::from_secs(5));
    let rejected = view.summarize("d");
    assert!(matches!(
        &*rejected,
        Err(api::Error::Admission(EnqueueError::Closed))
    ));
    assert!(matches!(
        rejected.await,
        Err(api::Error::Admission(EnqueueError::Closed))
    ));
    assert!(matches!(
        client.clone().login().await,
        Err(api::Error::Admission(EnqueueError::Closed))
    ));
    drop(view);
    drop(client);
    shutdown.await.unwrap();
    write.await.unwrap();
}

#[test]
fn bounded_admission_and_runtime_loss_do_not_depend_on_observer_polling() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut config = api::Config::new("token");
    config.runtime = Some(runtime.handle().clone());
    config.api_url = "http://127.0.0.1:9".into();
    let client = api::Client::new(config).unwrap();
    let mut accepted = Vec::new();
    for _ in 0..1024 {
        let token = client.logs3().version();
        assert!(token.is_ok());
        accepted.push(token);
    }
    drop(accepted.pop());
    let full = client.logs3().version();
    assert!(matches!(
        &*full,
        Err(api::Error::Admission(EnqueueError::Full))
    ));
    assert!(matches!(
        futures::executor::block_on(full.into_future()),
        Err(api::Error::Admission(EnqueueError::Full))
    ));
    let shutdown = client.shutdown(Duration::from_secs(5));
    drop(client);
    drop(runtime);
    for token in accepted {
        assert!(matches!(
            futures::executor::block_on(token.into_future()),
            Err(api::Error::Execution(ExecutionError::WorkerLost))
        ));
    }
    assert!(matches!(
        futures::executor::block_on(shutdown.into_future()),
        Err(ShutdownError::Execution(ExecutionError::WorkerLost))
    ));
}

#[tokio::test]
async fn first_shutdown_deadline_cancels_requests_and_is_shared_by_later_observers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({}))
                .set_delay(Duration::from_secs(2)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = api::Client::new(config(&server)).unwrap();
    let request = client.logs3().version();
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.received_requests().await.unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let first = client.shutdown(Duration::from_millis(20));
    let later = client.clone().shutdown(Duration::from_secs(10));
    for token in [first, later] {
        assert!(matches!(
            token.await,
            Err(ShutdownError::Execution(ExecutionError::TimedOut))
        ));
    }
    assert!(matches!(
        request.await,
        Err(api::Error::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        client.shutdown(Duration::from_secs(10)).await,
        Err(ShutdownError::Execution(ExecutionError::TimedOut))
    ));
}

#[tokio::test]
async fn request_errors_remain_on_their_tokens_and_do_not_poison_shutdown() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(500).set_body_string("failed"))
        .expect(1)
        .mount(&server)
        .await;
    let client = api::Client::new(config(&server)).unwrap();
    let request = client.logs3().version();
    let shutdown = client.shutdown(Duration::from_secs(5));
    shutdown.await.unwrap();
    assert!(matches!(
        request.await,
        Err(api::Error::Http { status: 500, .. })
    ));
    client.shutdown(Duration::ZERO).await.unwrap();
}
