use braintrust_sdk_rust::{
    eval::{Case, Dataset, Scorer, ScorerArgs, SimpleAsyncFnTask, Task, TaskHooks},
    BraintrustClient, DatasetInsert, Logs3BatchUploader, ParentSpanInfo,
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
        ("/api/project/register", json!({"project":{"id":"p"}})),
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
                .default_project("p")
                .app_url(&endpoint)
                .api_url(&endpoint)
                .blocking_login(true)
                .build()
                .blocking_wait()
                .unwrap();
            assert!(client.is_logged_in());
            assert_eq!(client.login_state().org_id().as_deref(), Some("org"));
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
            // Evaluations run user tasks, so sync code drives them on its own runtime.
            let summary = braintrust_sdk_rust::eval::SyncEvaluator::new(client.clone())
                .unwrap()
                .run(
                    braintrust_sdk_rust::eval::EvalOpts::builder()
                        .name("async-task-from-sync-code".into())
                        .dataset(
                            vec![Case {
                                input: "hello".to_owned(),
                                expected: Some("HELLO".to_owned()),
                                ..Default::default()
                            }]
                            .into(),
                        )
                        .task(Box::new(SimpleAsyncFnTask::new(|input: &String| {
                            Box::pin(async move {
                                tokio::task::yield_now().await;
                                Ok(input.to_uppercase())
                            })
                        })))
                        .scorers(vec![Box::new(braintrust_sdk_rust::eval::ExactMatch::new())])
                        .quiet(true)
                        .build(),
                )
                .unwrap();
            assert_eq!(summary.successful_cases, 1);
            assert_eq!(summary.results[0].output.as_deref(), Some("HELLO"));
            client.flush().blocking_wait().unwrap();
        })
        .join()
        .unwrap()
    })
    .await
    .unwrap();
}

// These implementations intentionally use the pre-existing #[async_trait] signatures.
// Non-Clone values and borrowed state catch accidental ownership restrictions.
struct Input(String);
struct Output(String);
struct BorrowedTask<'a>(&'a str);

#[async_trait::async_trait]
impl Task<Input, Output> for BorrowedTask<'_> {
    async fn run(
        &self,
        input: &Input,
        hooks: &TaskHooks<'_, Input, Output>,
    ) -> braintrust_sdk_rust::Result<Output> {
        tokio::task::yield_now().await;
        hooks.add_tag(self.0);
        Ok(Output(format!("{}{}", input.0, self.0)))
    }
}

struct BorrowedScorer<'a>(&'a str);
#[async_trait::async_trait]
impl Scorer<Input, Output> for BorrowedScorer<'_> {
    fn name(&self) -> &str {
        self.0
    }
    async fn score(
        &self,
        args: ScorerArgs<'_, Input, Output>,
    ) -> braintrust_sdk_rust::Result<braintrust_sdk_rust::eval::Scores> {
        tokio::task::yield_now().await;
        Ok(vec![braintrust_sdk_rust::eval::Score::new(
            self.0,
            f64::from(args.output.0.starts_with(&args.input.0)),
        )])
    }
}

struct BorrowedDataset<'a>(Option<&'a str>);
#[async_trait::async_trait]
impl<'a> Dataset<&'a str, &'a str> for BorrowedDataset<'a> {
    async fn next(&mut self) -> braintrust_sdk_rust::Result<Option<Case<&'a str, &'a str>>> {
        tokio::task::yield_now().await;
        Ok(self.0.take().map(|input| Case {
            input,
            expected: Some(input),
            ..Default::default()
        }))
    }
}

struct BorrowedProvider<'a>(&'a str);
#[async_trait::async_trait]
impl braintrust_sdk_rust::TokenProvider for BorrowedProvider<'_> {
    async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        tokio::task::yield_now().await;
        Ok(self.0.to_owned())
    }
}

#[tokio::test]
async fn extension_traits_preserve_borrowed_async_implementations() {
    use braintrust_sdk_rust::TokenProvider;
    let client = BraintrustClient::builder().build_without_login().unwrap();
    let span = client.span_builder_with_credentials("key", "org").build();
    let hooks = TaskHooks::new(&span, None);
    let input = Input("hello".to_owned());
    let suffix = "!".to_owned();
    let task = BorrowedTask(&suffix);
    let task: &dyn Task<Input, Output> = &task;
    let output = task.run(&input, &hooks).await.unwrap();
    assert_eq!(output.0, "hello!");
    assert_eq!(hooks.get_tags().unwrap(), ["!"]);
    let task = SimpleAsyncFnTask::new(|input: &Input| {
        let suffix = suffix.clone();
        Box::pin(async move {
            tokio::task::yield_now().await;
            Ok(Output(format!("{}{}", input.0, suffix)))
        })
    });
    assert_eq!(task.run(&input, &hooks).await.unwrap().0, "hello!");
    let scorer_name = "borrowed".to_owned();
    let scorer = BorrowedScorer(&scorer_name);
    let scorer: &dyn Scorer<Input, Output> = &scorer;
    let scores = scorer
        .score(ScorerArgs {
            input: &input,
            output: &output,
            expected: None,
            metadata: None,
        })
        .await
        .unwrap();
    assert_eq!(scores[0].name, "borrowed");
    assert_eq!(scores[0].score, 1.0);
    let text = "case".to_owned();
    let mut dataset = BorrowedDataset(Some(&text));
    assert_eq!(dataset.next().await.unwrap().unwrap().input, "case");
    assert!(dataset.next().await.unwrap().is_none());
    let provider = BorrowedProvider(&text);
    assert_eq!(provider.token().await.unwrap(), "case");
}
