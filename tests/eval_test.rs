use braintrust_sdk_rust::eval::CaseSource;
use braintrust_sdk_rust::eval::*;
use braintrust_sdk_rust::*;

async fn recording_client() -> (BraintrustClient, wiremock::MockServer) {
    use serde_json::json;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let server = MockServer::start().await;
    for (endpoint, response) in [
        (
            "/api/apikey/login",
            json!({"org_info":[{"id":"org","name":"org"}]}),
        ),
        ("/api/project/register", json!({"project":{"id":"project"}})),
        (
            "/api/experiment/register",
            json!({"project":{"id":"project","name":"project"},"experiment":{"id":"experiment","name":"experiment"}}),
        ),
        ("/logs3", json!({})),
    ] {
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .app_url(server.uri())
        .api_url(server.uri())
        .default_project("project")
        .blocking_login(true)
        .api_key("test-key")
        .build()
        .await
        .expect("Failed to create client");

    (client, server)
}

#[tokio::test]
async fn test_basic_eval_flow() {
    let (client, server) = recording_client().await;
    let evaluator = Evaluator::new(client);

    let cases = vec![
        Case {
            input: "hello".to_string(),
            expected: Some("HELLO".to_string()),
            ..Default::default()
        },
        Case {
            input: "world".to_string(),
            expected: Some("WORLD".to_string()),
            ..Default::default()
        },
    ];

    let callback = task(|input: &String, _hooks| Box::pin(async move { Ok(input.to_uppercase()) }));
    let scorers: Vec<Box<dyn Scorer<String, String>>> = vec![Box::new(scorer(
        "match",
        |args: ScorerArgs<'_, String, String>| {
            Box::pin(async move {
                Ok(vec![Score::new(
                    "match",
                    if args.expected == Some(args.output) {
                        1.0
                    } else {
                        0.0
                    },
                )])
            })
        },
    ))];

    let result = evaluator
        .run(
            EvalOpts::builder()
                .name("test-basic-eval".to_string())
                .source(cases.into())
                .definition(EvalDefinition::new(Box::new(callback), scorers).unwrap())
                .quiet(true)
                .build(),
        )
        .await
        .expect("Evaluation failed");

    assert_eq!(result.total_cases, 2);
    assert_eq!(result.successful_cases, 2);
    assert!(result
        .results
        .iter()
        .all(|case| case.span_id.is_some() && case.errors.is_empty()));
    let requests = server.received_requests().await.unwrap();
    assert!(requests
        .iter()
        .any(|request| request.url.path() == "/logs3"));
}

#[test]
fn test_eval_types_compile() {
    // Test that all the types and builders work correctly
    let cases = vec![Case {
        input: "test".to_string(),
        expected: Some("TEST".to_string()),
        ..Default::default()
    }];

    let _dataset: Box<dyn CaseSource<String, String>> = cases.into();
    let _task: Box<dyn Task<String, String>> =
        Box::new(SimpleFnTask::new(|input: &String| Ok(input.to_uppercase())));
    let _scorers: Vec<Box<dyn Scorer<String, String>>> = vec![Box::new(ExactMatch::new())];

    // Verify Score creation
    let score = Score::new("test_score", 0.95);
    assert_eq!(score.name, "test_score");
    assert_eq!(score.score, 0.95);
}

#[test]
fn test_case_default() {
    let case: Case<String, String> = Case {
        input: "hello".to_string(),
        ..Default::default()
    };

    assert_eq!(case.input, "hello");
    assert_eq!(case.expected, None);
    assert_eq!(case.metadata, None);
    assert_eq!(case.tags, None);
}

#[tokio::test]
async fn test_scorers() {
    // Test built-in scorers work correctly

    // ExactMatch
    let scorer = ExactMatch::new();
    let expected = String::from("TEST");
    let args = ScorerArgs {
        input: &"test".to_string(),
        output: &"TEST".to_string(),
        expected: Some(&expected),
        metadata: None,
    };
    let scores = scorer.score(args).await.unwrap();
    assert_eq!(scores[0].score, 1.0);

    // Levenshtein
    let scorer = LevenshteinScorer::new();
    let expected = String::from("hallo");
    let args = ScorerArgs {
        input: &"test".to_string(),
        output: &"hello".to_string(),
        expected: Some(&expected),
        metadata: None,
    };
    let scores = scorer.score(args).await.unwrap();
    assert!(scores[0].score > 0.7); // ~0.8 similarity

    // NumericDiff
    let scorer = NumericDiffScorer::new();
    let args = ScorerArgs {
        input: &"test".to_string(),
        output: &10.0,
        expected: Some(&8.0),
        metadata: None,
    };
    let scores = scorer.score(args).await.unwrap();
    assert_eq!(scores[0].score, 2.0); // absolute difference
}

#[test]
fn test_eval_opts_builder() {
    // Verify the builder pattern works
    let cases = vec![Case {
        input: "test".to_string(),
        expected: Some("TEST".to_string()),
        ..Default::default()
    }];

    let dataset: Box<dyn CaseSource<String, String>> = cases.into();
    let task: Box<dyn Task<String, String>> =
        Box::new(SimpleFnTask::new(|input: &String| Ok(input.to_uppercase())));
    let scorers: Vec<Box<dyn Scorer<String, String>>> = vec![Box::new(ExactMatch::new())];

    let _opts = EvalOpts::builder()
        .name("test-eval".to_string())
        .source(dataset)
        .definition(EvalDefinition::new(task, scorers).unwrap())
        .max_concurrency(10)
        .quiet(true)
        .build();
}

#[tokio::test]
async fn recorded_scorer_failure_keeps_valid_scores_and_finishes_the_case() {
    let (client, server) = recording_client().await;
    let evaluator = Evaluator::new(client);
    let scorers: Vec<Box<dyn Scorer<String, String>>> = vec![
        Box::new(ExactMatch::new()),
        Box::new(SyncFnScorer::new("broken", |_| {
            anyhow::bail!("scoring failed")
        })),
    ];
    let result = evaluator
        .run(
            EvalOpts::builder()
                .name("failed-scorer".into())
                .source(
                    vec![Case {
                        input: "a".to_owned(),
                        expected: Some("A".to_owned()),
                        id: Some("source-case".into()),
                        ..Default::default()
                    }]
                    .into(),
                )
                .definition(
                    EvalDefinition::new(
                        Box::new(SimpleFnTask::new(|input: &String| Ok(input.to_uppercase()))),
                        scorers,
                    )
                    .unwrap(),
                )
                .quiet(true)
                .build(),
        )
        .await
        .unwrap();
    assert_eq!(result.successful_cases, 0);
    assert_eq!(result.failed_cases, 1);
    assert_eq!(result.score_stats[0].name, "exact_match");
    assert_eq!(result.score_stats[0].mean, 1.0);
    let case = &result.results[0];
    assert_eq!(case.case_id.as_deref(), Some("source-case"));
    assert!(matches!(&case.errors[0], CaseError::Scorer {name,..} if name == "broken"));
    let rows: Vec<serde_json::Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/logs3")
        .flat_map(|request| {
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()["rows"]
                .as_array()
                .unwrap()
                .clone()
        })
        .collect();
    assert!(rows
        .iter()
        .any(|row| row["error"][0]["stage"] == "scorer" && row["error"][0]["name"] == "broken"));
    assert!(rows
        .iter()
        .any(|row| row["span_id"].as_str() == case.span_id.as_deref()
            && row["metrics"]["end"].is_number()));
}

#[tokio::test]
async fn source_failure_drains_active_cases_preserves_provenance_and_finishes_recording() {
    use async_trait::async_trait;
    use std::{
        cell::Cell,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };
    use tokio::sync::{Notify, Semaphore};
    #[derive(serde::Serialize)]
    struct Input(usize);
    #[derive(serde::Serialize)]
    struct Output(usize);
    #[derive(Debug, thiserror::Error)]
    #[error("source read failed")]
    struct ReadFailure;
    struct Source {
        reads: Cell<usize>,
        started: Arc<Notify>,
        failed: Arc<Notify>,
    }
    #[async_trait]
    impl CaseSource<Input, Output> for Source {
        async fn next(&mut self) -> anyhow::Result<Option<Case<Input, Output>>> {
            let index = self.reads.get();
            self.reads.set(index + 1);
            if index == 1 {
                self.started.notified().await;
            }
            if index == 2 {
                self.failed.notify_one();
                return Err(ReadFailure.into());
            }
            Ok(Some(Case {
                input: Input(index),
                expected: None,
                metadata: None,
                tags: None,
                id: Some(format!("case-{index}")),
                xact_id: Some(format!("row-version-{index}")),
                created: None,
            }))
        }
        fn provenance(&self) -> Option<DatasetVersionRef> {
            Some(DatasetVersionRef {
                dataset_id: "source-dataset".into(),
                version: Some(format!("version-{}", self.reads.get())),
            })
        }
    }
    let (client, server) = recording_client().await;
    let started = Arc::new(Notify::new());
    let failed = Arc::new(Notify::new());
    let second_done = Arc::new(Notify::new());
    let gates = Arc::new([Semaphore::new(0), Semaphore::new(0)]);
    let calls = Arc::new(AtomicUsize::new(0));
    let callback = {
        let started = started.clone();
        let second_done = second_done.clone();
        let gates = gates.clone();
        let calls = calls.clone();
        task(move |input: &Input, _hooks| {
            let started = started.clone();
            let second_done = second_done.clone();
            let gates = gates.clone();
            let calls = calls.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                if input.0 == 0 {
                    started.notify_one();
                }
                gates[input.0].acquire().await.unwrap().forget();
                if input.0 == 1 {
                    second_done.notify_one();
                }
                Ok(Output(input.0 * 2))
            })
        })
    };
    let opts = EvalOpts::builder()
        .name("source-failure".into())
        .source(Box::new(Source {
            reads: Cell::new(0),
            started,
            failed: failed.clone(),
        }) as Box<dyn CaseSource<Input, Output>>)
        .definition(EvalDefinition::new(Box::new(callback), vec![]).unwrap())
        .max_concurrency(3)
        .quiet(true)
        .build();
    let run = tokio::spawn(async move { Evaluator::new(client).run(opts).await });
    tokio::time::timeout(std::time::Duration::from_secs(3), failed.notified())
        .await
        .expect("pending source reads must allow active task progress");
    gates[1].add_permits(1);
    second_done.notified().await;
    assert!(
        !run.is_finished(),
        "source failure must still wait for the other scheduled case"
    );
    gates[0].add_permits(1);
    let error = match tokio::time::timeout(std::time::Duration::from_secs(3), run)
        .await
        .unwrap()
        .unwrap()
    {
        Err(error) => error,
        Ok(_) => panic!("source failure must not become a successful summary"),
    };
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let [EvalRunFailure::Source { case_index, source }] = error.failures.as_slice() else {
        panic!("expected a typed source failure")
    };
    assert_eq!(*case_index, 2);
    assert!(source.downcast_ref::<ReadFailure>().is_some());
    let partial = error.partial.unwrap();
    assert_eq!(partial.total_cases, 2);
    assert_eq!(partial.successful_cases, 2);
    assert_eq!(partial.failed_cases, 0);
    let dataset = partial.dataset.unwrap();
    assert_eq!(dataset.dataset_id, "source-dataset");
    assert_eq!(dataset.version.as_deref(), Some("version-0"));
    assert_eq!(
        partial
            .results
            .iter()
            .map(|case| case.input.0)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(partial.results[1].case_id.as_deref(), Some("case-1"));
    assert_eq!(partial.results[1].xact_id.as_deref(), Some("row-version-1"));
    assert_eq!(partial.results[1].output.as_ref().unwrap().0, 2);
    let rows: Vec<serde_json::Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/logs3")
        .flat_map(|request| {
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()["rows"]
                .as_array()
                .unwrap()
                .clone()
        })
        .collect();
    let ended: std::collections::HashSet<_> = rows
        .iter()
        .filter(|row| row["metrics"]["end"].is_number())
        .map(|row| row["span_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ended.len(),
        2,
        "both case roots must finish before returning partial results"
    );
}

#[tokio::test]
async fn failure_before_the_first_case_returns_the_resolved_target_without_trace_rows() {
    let (client, server) = recording_client().await;
    let items: Vec<anyhow::Result<Case<String, String>>> =
        vec![Err(anyhow::anyhow!("source unavailable"))];
    let opts = EvalOpts::builder()
        .name("empty-failure".into())
        .source(
            Box::new(IterCaseSource::new(items.into_iter())) as Box<dyn CaseSource<String, String>>
        )
        .definition(
            EvalDefinition::new(
                Box::new(SimpleFnTask::new(|_: &String| -> anyhow::Result<String> {
                    panic!("no case was read")
                })),
                vec![],
            )
            .unwrap(),
        )
        .quiet(true)
        .build();
    let error = Evaluator::new(client).run(opts).await.unwrap_err();
    assert!(
        matches!(&error.failures[0],EvalRunFailure::Source {case_index:0,source} if source.to_string()=="source unavailable")
    );
    let partial = error.partial.unwrap();
    assert_eq!(partial.total_cases, 0);
    assert_eq!(partial.successful_cases, 0);
    assert_eq!(partial.failed_cases, 0);
    assert!(partial.results.is_empty());
    assert!(partial.dataset.is_none());
    assert_eq!(partial.recording.unwrap().experiment_id, "experiment");
    assert!(!server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|request| request.url.path() == "/logs3"));
}

#[tokio::test]
async fn recording_failure_stops_new_cases_but_drains_already_scheduled_cases() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::sync::{Notify, Semaphore};
    use wiremock::{
        matchers::{method, path},
        Mock, ResponseTemplate,
    };
    let (client, server) = recording_client().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(400))
        .with_priority(1)
        .mount(&server)
        .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let gate = Arc::new(Semaphore::new(0));
    let callback = {
        let calls = calls.clone();
        let entered = entered.clone();
        let gate = gate.clone();
        task(move |input: &usize, _hooks| {
            let calls = calls.clone();
            let entered = entered.clone();
            let gate = gate.clone();
            Box::pin(async move {
                if calls.fetch_add(1, Ordering::SeqCst) == 1 {
                    entered.notify_one();
                }
                gate.acquire().await.unwrap().forget();
                Ok(*input)
            })
        })
    };
    let opts = EvalOpts::builder()
        .name("record-failure".into())
        .source(
            (0..3)
                .map(|input| Case {
                    input,
                    ..Default::default()
                })
                .collect::<Vec<Case<usize, usize>>>()
                .into(),
        )
        .definition(EvalDefinition::new(Box::new(callback), vec![]).unwrap())
        .max_concurrency(2)
        .quiet(true)
        .build();
    let run = tokio::spawn(async move { Evaluator::new(client).run(opts).await });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    gate.add_permits(2);
    let error = tokio::time::timeout(std::time::Duration::from_secs(3), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the third case must remain unread after recording failure"
    );
    assert!(error.failures.iter().any(|failure| matches!(
        failure,
        EvalRunFailure::Recording {
            case_index: Some(0),
            ..
        }
    )));
    assert!(error.failures.iter().any(|failure| matches!(
        failure,
        EvalRunFailure::Recording {
            case_index: Some(1),
            ..
        }
    )));
    let partial = error.partial.unwrap();
    assert_eq!(partial.total_cases, 2);
    assert_eq!(partial.failed_cases, 0);
    assert_eq!(partial.successful_cases, 2);
    assert_eq!(partial.results.len(), 2);
    assert_eq!(
        partial
            .results
            .iter()
            .map(|result| result.output)
            .collect::<Vec<_>>(),
        [Some(0), Some(1)]
    );
    assert!(partial
        .results
        .iter()
        .all(|result| result.errors.is_empty() && result.span_id.is_some()));
}

#[tokio::test]
async fn reusable_definition_keeps_recorded_runs_and_sources_independent() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let (client, _server) = recording_client().await;
    let evaluator = Evaluator::new(client);
    let calls = Arc::new(AtomicUsize::new(0));
    let definition = EvalDefinition::new(
        Box::new(task({
            let calls = calls.clone();
            move |input: &usize, hooks| {
                calls.fetch_add(1, Ordering::SeqCst);
                hooks.add_tag(format!("input-{input}"));
                Box::pin(async move { Ok(*input * 2) })
            }
        })),
        vec![Box::new(ExactMatch::new())],
    )
    .unwrap();
    let options = |name: &str, input, expected| {
        EvalOpts::builder()
            .name(name.into())
            .source(
                vec![Case {
                    input,
                    expected: Some(expected),
                    ..Default::default()
                }]
                .into(),
            )
            .definition(definition.clone())
            .quiet(true)
            .build()
    };
    let (first, second) = tokio::join!(
        evaluator.run(options("first", 2, 4)),
        evaluator.run(options("second", 3, 100)),
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.name, "first");
    assert_eq!(second.name, "second");
    assert_eq!(first.results[0].output, Some(4));
    assert_eq!(second.results[0].output, Some(6));
    assert_eq!(first.results[0].scores[0].score, 1.0);
    assert_eq!(second.results[0].scores[0].score, 0.0);
    assert_eq!(first.results[0].tags, Some(vec!["input-2".into()]));
    assert_eq!(second.results[0].tags, Some(vec!["input-3".into()]));
    assert_ne!(first.results[0].span_id, second.results[0].span_id);
    let later = evaluator.run(options("later", 5, 10)).await.unwrap();
    assert_eq!(later.total_cases, 1);
    assert_eq!(later.results[0].output, Some(10));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn serialization_failures_keep_outputs_and_finalize_recording() {
    #[derive(Debug)]
    struct Value(usize);
    impl serde::Serialize for Value {
        fn serialize<S: serde::Serializer>(&self, _: S) -> std::result::Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom(
                "application serialization failed",
            ))
        }
    }
    let (client, server) = recording_client().await;
    let options = EvalOpts::builder()
        .name("serialization-failure".into())
        .source(
            vec![Case {
                input: Value(7),
                expected: Some(Value(14)),
                metadata: None,
                tags: None,
                id: None,
                xact_id: None,
                created: None,
            }]
            .into(),
        )
        .definition(
            EvalDefinition::new(
                Box::new(SimpleFnTask::new(|input: &Value| Ok(Value(input.0 * 2)))),
                vec![],
            )
            .unwrap(),
        )
        .quiet(true)
        .build();
    let error = Evaluator::new(client).run(options).await.unwrap_err();
    let fields = error
        .failures
        .iter()
        .map(|failure| match failure {
            EvalRunFailure::Recording {
                case_index: Some(0),
                source,
            } => source.to_string(),
            other => panic!("unexpected failure: {other}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(fields.len(), 3);
    for (field, message) in ["input", "expected", "output"].into_iter().zip(fields) {
        assert!(message.contains(field));
        assert!(message.contains("application serialization failed"));
    }
    let summary = error.partial.unwrap();
    assert_eq!(summary.successful_cases, 1);
    assert_eq!(summary.failed_cases, 0);
    assert_eq!(summary.results[0].output.as_ref().unwrap().0, 14);
    assert!(summary.results[0].span_id.is_some());
    let requests = server.received_requests().await.unwrap();
    let ended: std::collections::HashSet<_> = requests
        .iter()
        .filter(|request| request.url.path() == "/logs3")
        .flat_map(|request| {
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()["rows"]
                .as_array()
                .unwrap()
                .clone()
        })
        .filter(|row| row["metrics"]["end"].is_number())
        .map(|row| row["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ended.len(), 1, "case recording must finalize");
}

#[tokio::test]
async fn experiment_is_resolved_before_source_reads_and_case_roots_keep_independent_traces() {
    use serde_json::json;
    use std::{
        cell::Cell,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };
    use wiremock::{
        matchers::{header, method, path},
        Mock, ResponseTemplate,
    };
    let (client, server) = recording_client().await;
    let registered = Arc::new(AtomicBool::new(false));
    let prepared = Arc::new(AtomicBool::new(false));
    Mock::given(method("POST")).and(path("/api/experiment/register"))
        .and(header("authorization", "Bearer test-key"))
        .and(header("x-bt-org-name", "org"))
        .respond_with({
            let registered = registered.clone();
            let prepared = prepared.clone();
            move |_: &wiremock::Request| {
                assert!(prepared.load(Ordering::SeqCst));
                registered.store(true, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_json(json!({"project":{"id":"chosen-project","name":"chosen"},"experiment":{"id":"chosen-id","name":"resolved-name"}}))
            }
        }).with_priority(1).expect(1).mount(&server).await;
    struct Source {
        cursor: Cell<usize>,
        registered: Arc<AtomicBool>,
        prepared: Arc<AtomicBool>,
    }
    #[async_trait::async_trait]
    impl CaseSource<usize, usize> for Source {
        async fn prepare(&mut self) -> anyhow::Result<()> {
            assert!(
                !self.prepared.swap(true, Ordering::SeqCst),
                "prepare exactly once"
            );
            assert!(!self.registered.load(Ordering::SeqCst));
            Ok(())
        }
        async fn next(&mut self) -> anyhow::Result<Option<Case<usize, usize>>> {
            assert!(self.registered.load(Ordering::SeqCst));
            let index = self.cursor.get();
            self.cursor.set(index + 1);
            Ok((index < 2).then(|| Case {
                input: index,
                id: Some(format!("application-case-{index}")),
                xact_id: Some("application-version".into()),
                metadata: Some(json!({"collision":"case"}).as_object().unwrap().clone()),
                tags: Some(vec!["shared".into(), "case".into()]),
                ..Default::default()
            }))
        }
    }
    let definition = EvalDefinition::new(
        Box::new(task(|input: &usize, hooks| {
            Box::pin(async move {
                assert_eq!(hooks.get_metadata().unwrap()["shared"], "run");
                assert_eq!(hooks.get_metadata().unwrap()["collision"], "case");
                let child = hooks
                    .span
                    .unwrap()
                    .child_builder()
                    .unwrap()
                    .name("nested")
                    .build();
                child
                    .log(SpanLog::builder().input(json!(input)).build().unwrap())
                    .await?;
                child.end().await?;
                hooks.add_tag("task");
                Ok(*input * 2)
            })
        })),
        vec![],
    )
    .unwrap();
    let opts = EvalOpts::builder()
        .name("named-eval".into())
        .source(Box::new(Source {
            cursor: Cell::new(0),
            registered,
            prepared,
        }))
        .definition(definition)
        .project_name("chosen".into())
        .experiment_name("requested-name".into())
        .metadata(
            json!({"shared":"run", "collision":"run"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .tags(vec!["run".into(), "shared".into()])
        .quiet(true)
        .build();
    let summary = run(EvalRequest::recorded(opts, client)).await.unwrap();
    assert_eq!(
        summary.recording.as_ref().unwrap(),
        &EvalRecording {
            project_id: "chosen-project".into(),
            experiment_id: "chosen-id".into(),
            experiment_name: "resolved-name".into(),
        }
    );
    assert_eq!(
        summary.results[0].tags.as_ref().unwrap(),
        &["run", "shared", "case", "task"]
    );
    let requests = server.received_requests().await.unwrap();
    assert!(!requests
        .iter()
        .any(|request| request.url.path() == "/api/project/register"));
    let registration: serde_json::Value = serde_json::from_slice(
        &requests
            .iter()
            .find(|request| request.url.path() == "/api/experiment/register")
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(registration["project_name"], "chosen");
    assert_eq!(registration["experiment_name"], "requested-name");
    assert_eq!(registration["metadata"]["collision"], "run");
    assert!(registration.get("dataset_id").is_none());
    assert!(registration.get("dataset_version").is_none());
    let rows: Vec<serde_json::Value> = requests
        .iter()
        .filter(|request| request.url.path() == "/logs3")
        .flat_map(|request| {
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()["rows"]
                .as_array()
                .unwrap()
                .clone()
        })
        .collect();
    assert!(rows
        .iter()
        .all(|row| row["experiment_id"] == "chosen-id" && row.get("project_id").is_none()));
    assert!(rows
        .iter()
        .all(|row| row["span_attributes"]["name"] != "eval-named-eval"));
    assert!(rows.iter().all(|row| row.get("origin").is_none()));
    let mut roots = std::collections::HashSet::new();
    for case in &summary.results {
        let id = case.span_id.as_ref().unwrap();
        let root = rows
            .iter()
            .find(|row| row["span_id"] == *id && row["metrics"]["end"].is_number())
            .unwrap();
        assert_eq!(root["root_span_id"], *id);
        roots.insert(id.clone());
        let child = rows
            .iter()
            .find(|row| row["span_attributes"]["name"] == "nested" && row["root_span_id"] == *id)
            .unwrap();
        assert_eq!(child["span_parents"][0], *id);
    }
    assert_eq!(roots.len(), 2);
    assert_eq!(
        summary_to_value(&summary).unwrap()["recording"]["experiment_id"],
        "chosen-id"
    );
}

#[tokio::test]
async fn registration_failure_reads_no_cases_and_a_later_run_can_record_normally() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use wiremock::{
        matchers::{method, path},
        Mock, ResponseTemplate,
    };
    let (client, server) = recording_client().await;
    Mock::given(method("POST"))
        .and(path("/api/experiment/register"))
        .respond_with(ResponseTemplate::new(403).set_body_string("registration denied"))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    let reads = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let definition = EvalDefinition::new(
        Box::new(SimpleFnTask::new({
            let calls = calls.clone();
            move |input: &usize| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(*input)
            }
        })),
        vec![],
    )
    .unwrap();
    let opts = EvalOpts::builder()
        .name("blocked".into())
        .source(Box::new(IterCaseSource::new({
            let reads = reads.clone();
            (0..1).map(move |input| {
                reads.fetch_add(1, Ordering::SeqCst);
                Ok(Case {
                    input,
                    ..Default::default()
                })
            })
        })))
        .definition(definition.clone())
        .quiet(true)
        .build();
    let error = run(EvalRequest::recorded(opts, client.clone()))
        .await
        .unwrap_err();
    assert!(matches!(
        error.failures.as_slice(),
        [EvalRunFailure::Recording {
            case_index: None,
            source: BraintrustError::Api { status: 403, .. }
        }]
    ));
    assert_eq!(error.partial.as_ref().unwrap().total_cases, 0);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|request| request.url.path() == "/logs3"));
    let opts = EvalOpts::builder()
        .name("later".into())
        .source(
            vec![Case {
                input: 5,
                ..Default::default()
            }]
            .into(),
        )
        .definition(definition)
        .quiet(true)
        .build();
    let summary = run(EvalRequest::recorded(opts, client.clone()))
        .await
        .unwrap();
    assert_eq!(summary.results[0].output, Some(5));
    assert_eq!(summary.recording.unwrap().experiment_id, "experiment");
    client
        .shutdown(std::time::Duration::from_secs(5))
        .await
        .unwrap();
}
