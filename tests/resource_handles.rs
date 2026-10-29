//! Consumer coverage for the concrete public resource model.
use braintrust_sdk_rust::{
    BraintrustClient, Dataset, DatasetBuilder, DatasetInsert, DatasetIterator, Experiment,
    ExperimentBuilder, ExperimentSpanBuilder, ParentSpanInfo, SpanBuilder, SpanHandle, SpanLog,
};
use serde_json::json;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

// Applications can store these types without naming an SDK implementation or
// carrying its private bounds through their own structs and functions.
struct Resources {
    span: SpanHandle,
    experiment: Experiment,
    dataset: Dataset,
}

#[test]
fn resource_handles_and_builders_are_send_sync_and_owned() {
    fn assert_owned<T: Send + Sync + 'static>() {}
    assert_owned::<SpanHandle>();
    assert_owned::<SpanBuilder>();
    assert_owned::<Experiment>();
    assert_owned::<ExperimentBuilder>();
    assert_owned::<ExperimentSpanBuilder>();
    assert_owned::<Dataset>();
    assert_owned::<DatasetBuilder>();
    assert_owned::<DatasetIterator>();
}

#[tokio::test]
async fn resources_and_detached_span_builders_work_after_the_client_is_dropped() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/experiment/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"project":{"id":"project","name":"p"},"experiment":{"id":"experiment","name":"e"}})))
        .expect(1).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"project":{"id":"project","name":"p"},"dataset":{"id":"dataset","name":"d"}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .expect(3)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"record"}]})))
        .expect(1)
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .skip_login(true)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap();
    let span: SpanBuilder = client
        .span_builder_with_credentials("token", "org")
        .parent_info(ParentSpanInfo::ProjectLogs {
            object_id: "project".into(),
        });
    let experiment = client
        .experiment_builder_with_credentials("token", "org")
        .project_name("p");
    let dataset = client
        .dataset_builder_with_credentials("token", "tenant")
        .project_name("p")
        .dataset_name("d");
    drop(client);
    let resources = Resources {
        span: span.build(),
        experiment: experiment.build(),
        dataset: dataset.build(),
    };
    resources.span.log(SpanLog::default()).await.unwrap();
    let child: ExperimentSpanBuilder = resources.experiment.start_span().await.unwrap();
    drop(resources.experiment);
    let child: SpanHandle = child.build();
    child.log(SpanLog::default()).await.unwrap();
    resources
        .dataset
        .insert(DatasetInsert::builder().input(json!(1)).build())
        .await
        .unwrap();
    let records: DatasetIterator = resources.dataset.fetch(None).await.unwrap();
    drop(resources.dataset);
    assert_eq!(records.collect().await.unwrap()[0].id(), "record");
    resources.span.flush().await.unwrap();
}

#[tokio::test]
async fn dropping_the_last_resource_and_client_still_drains_an_accepted_write() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .skip_login(true)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap();
    let span: SpanHandle = client
        .span_builder_with_credentials("token", "org")
        .parent_info(ParentSpanInfo::ProjectLogs {
            object_id: "project".into(),
        })
        .build();
    let write = span.log(SpanLog::default());
    assert!(write.is_ok());
    drop(span);
    drop(client);
    tokio::time::timeout(std::time::Duration::from_secs(5), async { write.await })
        .await
        .unwrap()
        .unwrap();
}
