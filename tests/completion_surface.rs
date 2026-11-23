use braintrust_sdk_rust::{
    eval::{
        Case, Dataset, ExactMatch, Scorer, ScorerArgs, SimpleAsyncFnTask, Task, TaskHooks,
        VecDataset,
    },
    BraintrustClient, Completion, DatasetInsert, Logs3BatchUploader, ParentSpanInfo,
};
use serde_json::json;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn high_level_operations_work_from_a_plain_sync_thread() {
    let server = MockServer::start().await;
    for (route, response) in [
        (
            "/api/apikey/login",
            json!({"org_info":[{"id":"org", "name":"org"}]}),
        ),
        (
            "/api/dataset/register",
            json!({"project":{"id":"p","name":"p"}, "dataset":{"id":"d","name":"d"}}),
        ),
        (
            "/api/experiment/register",
            json!({"project":{"id":"p","name":"p"}, "experiment":{"id":"e","name":"e"}}),
        ),
        ("/logs3", json!({})),
        ("/btql", json!({"data":[{"id":"record","input":"hello"}]})),
    ] {
        Mock::given(method("POST"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .mount(&server)
            .await;
    }
    let endpoint = server.uri();
    tokio::task::spawn_blocking(move || {
        std::thread::spawn(move || {
            let client = BraintrustClient::builder()
                .api_key("key")
                .app_url(&endpoint)
                .api_url(&endpoint)
                .blocking_login(true)
                .build()
                .blocking_wait()
                .unwrap();
            assert!(client.is_logged_in().blocking_wait().unwrap());
            assert_eq!(
                client
                    .login_state()
                    .blocking_wait()
                    .unwrap()
                    .org_id()
                    .as_deref(),
                Some("org")
            );
            let span = client
                .span_builder()
                .blocking_wait()
                .unwrap()
                .parent_info(ParentSpanInfo::ProjectLogs {
                    object_id: "p".into(),
                })
                .build();
            span.log(Default::default()).blocking_wait().unwrap();
            let selected = client.with_organization("org").blocking_wait().unwrap();
            selected.wait_for_login().blocking_wait().unwrap();
            let dataset = client
                .dataset_builder()
                .blocking_wait()
                .unwrap()
                .project_name("p")
                .dataset_name("d")
                .build()
                .unwrap();
            dataset
                .insert(
                    DatasetInsert::builder()
                        .input(json!("hello"))
                        .build()
                        .unwrap(),
                )
                .blocking_wait()
                .unwrap();
            dataset.flush().blocking_wait().unwrap();
            let mut records = dataset.fetch(Some(1)).blocking_wait().unwrap();
            assert_eq!(
                records.next().blocking_wait().unwrap().unwrap().id,
                "record"
            );
            assert!(records.next().blocking_wait().unwrap().is_none());
            let experiment = client
                .experiment_builder()
                .blocking_wait()
                .unwrap()
                .project_name("p")
                .experiment_name("e")
                .build()
                .unwrap();
            let builder = experiment.start_span().blocking_wait().unwrap();
            builder
                .build()
                .log(Default::default())
                .blocking_wait()
                .unwrap();
            experiment.export().blocking_wait().unwrap();
            let uploader = Logs3BatchUploader::new(endpoint, "key", None).unwrap();
            uploader
                .upload_rows(
                    &[json!({"id":"bulk","project_id":"p"})
                        .as_object()
                        .unwrap()
                        .clone()],
                    10,
                )
                .blocking_wait()
                .unwrap();
            client.flush().blocking_wait().unwrap();
        })
        .join()
        .unwrap()
    })
    .await
    .unwrap();
}

#[test]
fn evaluation_traits_support_sync_observation_and_owned_async_callbacks() {
    let client = BraintrustClient::builder().build_without_login().unwrap();
    let span = client.span_builder_with_credentials("key", "org").build();
    let expected = "HELLO".to_owned();
    let hooks = TaskHooks::new(&span, Some(&expected));
    let task =
        SimpleAsyncFnTask::new(|input: &String| Box::pin(async move { Ok(input.to_uppercase()) }));
    let input = "hello".to_owned();
    let output = task.run(&input, &hooks).blocking_wait().unwrap();
    assert_eq!(output, "HELLO");
    let scores = ExactMatch::new()
        .score(ScorerArgs {
            input: &input,
            output: &output,
            expected: Some(&output),
            metadata: None,
        })
        .blocking_wait()
        .unwrap();
    assert_eq!(scores[0].score, 1.0);
    let mut dataset = VecDataset::new(vec![Case {
        input,
        expected: Some(output),
        ..Default::default()
    }]);
    assert!(dataset.next().blocking_wait().unwrap().is_some());
    assert!(dataset.next().blocking_wait().unwrap().is_none());
}

#[tokio::test]
async fn dropping_a_completion_does_not_cancel_its_operation() {
    let (sent, received) = tokio::sync::oneshot::channel();
    drop(
        Completion::<(), braintrust_sdk_rust::BraintrustError>::spawn(async move {
            tokio::task::yield_now().await;
            sent.send(()).unwrap();
            Ok(())
        }),
    );
    received.await.unwrap();
}
