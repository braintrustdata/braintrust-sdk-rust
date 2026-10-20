use braintrust_sdk_rust::{eval::*, BraintrustClient};
use serde::{Serialize, Serializer};
use serde_json::json;
use std::{
    cell::Cell,
    io::{self, Write},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);
impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn options(calls: Arc<AtomicUsize>) -> EvalOpts<usize, usize> {
    EvalOpts::builder()
        .name("reporting".into())
        .quiet(true)
        .max_concurrency(1)
        .source(
            (0..4)
                .map(|input| Case {
                    input,
                    ..Default::default()
                })
                .collect::<Vec<_>>()
                .into(),
        )
        .definition(
            EvalDefinition::new(
                Box::new(SimpleFnTask::new(move |input: &usize| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(input * 2)
                })),
                vec![],
            )
            .unwrap(),
        )
        .build()
}
struct FailedReporter {
    stage: ReportStage,
    attempts: Arc<AtomicUsize>,
}
impl FailedReporter {
    fn report(&self, stage: ReportStage) -> Result<(), ReportError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.stage == stage {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed output").into())
        } else {
            Ok(())
        }
    }
}
impl EvalReporter<usize, usize> for FailedReporter {
    fn started(&mut self, _: &str) -> Result<(), ReportError> {
        self.report(ReportStage::Started)
    }
    fn case_finished(&mut self, _: usize, _: &EvalResult<usize, usize>) -> Result<(), ReportError> {
        self.report(ReportStage::CaseFinished)
    }
    fn finished(&mut self, _: &EvalSummary<usize, usize>) -> Result<(), ReportError> {
        self.report(ReportStage::Finished)
    }
}
#[test]
fn reporting_errors_disable_output_without_stopping_cases_or_losing_results() {
    for stage in [
        ReportStage::Started,
        ReportStage::CaseFinished,
        ReportStage::Finished,
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let attempts = Arc::new(AtomicUsize::new(0));
        let completion = run(EvalRequest::local(options(calls.clone())).with_reporter(
            FailedReporter {
                stage,
                attempts: attempts.clone(),
            },
        ));
        assert!(completion.is_ok());
        let Err(error) = futures::executor::block_on(async { completion.await }) else {
            panic!("reporting must fail");
        };
        let [EvalRunFailure::Reporting {
            stage: actual,
            case_index,
            source: ReportError::Io(source),
        }] = error.failures.as_slice()
        else {
            panic!("unexpected failures")
        };
        assert_eq!(*actual, stage);
        assert_eq!(
            *case_index,
            (stage == ReportStage::CaseFinished).then_some(0)
        );
        assert_eq!(source.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            match stage {
                ReportStage::Started => 1,
                ReportStage::CaseFinished => 2,
                ReportStage::Finished => 6,
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        let summary = error.partial.unwrap();
        assert_eq!(summary.successful_cases, 4);
        assert_eq!(
            summary
                .results
                .iter()
                .map(|r| r.output.unwrap())
                .collect::<Vec<_>>(),
            [0, 2, 4, 6]
        );
    }
}
#[test]
fn custom_local_reporter_needs_no_serialization_clone_debug_or_sync() {
    struct Input(usize);
    struct Output(usize);
    struct Reporter {
        count: Cell<usize>,
        observed: Arc<AtomicUsize>,
    }
    impl EvalReporter<Input, Output> for Reporter {
        fn case_finished(
            &mut self,
            index: usize,
            result: &EvalResult<Input, Output>,
        ) -> Result<(), ReportError> {
            assert_eq!(index, 0);
            assert_eq!(result.output.as_ref().unwrap().0, 9);
            self.count.set(self.count.get() + 1);
            Ok(())
        }
        fn finished(&mut self, _: &EvalSummary<Input, Output>) -> Result<(), ReportError> {
            self.observed.store(self.count.get(), Ordering::SeqCst);
            Ok(())
        }
    }
    let observed = Arc::new(AtomicUsize::new(0));
    let opts = EvalOpts::builder()
        .name("opaque".into())
        .source(
            vec![Case {
                input: Input(3),
                expected: None,
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
                Box::new(SimpleFnTask::new(|input: &Input| Ok(Output(input.0 * 3)))),
                vec![],
            )
            .unwrap(),
        )
        .build();
    let completion = run(EvalRequest::local(opts).with_reporter(Reporter {
        count: Cell::new(0),
        observed: observed.clone(),
    }));
    futures::executor::block_on(async { completion.await }).unwrap();
    assert_eq!(observed.load(Ordering::SeqCst), 1);
}
#[test]
fn jsonl_outputs_one_complete_summary_and_terminal_uses_separate_writers() {
    let output = Buffer::default();
    let completion = run(EvalRequest::local(options(Arc::new(AtomicUsize::new(0))))
        .with_reporter(JsonlReporter::new(output.clone())));
    futures::executor::block_on(async { completion.await }).unwrap();
    let bytes = output.0.lock().unwrap();
    assert_eq!(bytes.iter().filter(|&&b| b == b'\n').count(), 1);
    let summary: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(summary["total_cases"], 4);
    drop(bytes);
    let terminal = Buffer::default();
    let errors = Buffer::default();
    let mut opts = options(Arc::new(AtomicUsize::new(0)));
    opts.definition = EvalDefinition::new(
        Box::new(SimpleFnTask::new(|_: &usize| -> anyhow::Result<usize> {
            anyhow::bail!("task-error")
        })),
        vec![],
    )
    .unwrap();
    let completion = run(
        EvalRequest::local(opts).with_reporter(TerminalReporter::with_writers(
            terminal.clone(),
            errors.clone(),
        )),
    );
    assert_eq!(
        futures::executor::block_on(async { completion.await })
            .unwrap()
            .failed_cases,
        4
    );
    assert!(String::from_utf8(terminal.0.lock().unwrap().clone())
        .unwrap()
        .contains("Failed: 4"));
    assert!(String::from_utf8(errors.0.lock().unwrap().clone())
        .unwrap()
        .contains("task-error"));
}
struct Unserializable;
impl Serialize for Unserializable {
    fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("cannot serialize"))
    }
}
#[test]
fn serialization_errors_preserve_computed_results_without_writing_fake_json() {
    let output = Buffer::default();
    let opts = EvalOpts::builder()
        .name("serialize".into())
        .source(
            vec![Case {
                input: Unserializable,
                expected: None,
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
                Box::new(SimpleFnTask::new(|_: &Unserializable| Ok(7usize))),
                vec![],
            )
            .unwrap(),
        )
        .build();
    let completion =
        run(EvalRequest::local(opts).with_reporter(JsonlReporter::new(output.clone())));
    let Err(error) = futures::executor::block_on(async { completion.await }) else {
        panic!("reporting must fail");
    };
    assert!(matches!(
        error.failures.as_slice(),
        [EvalRunFailure::Reporting {
            stage: ReportStage::Finished,
            source: ReportError::Serialization(_),
            ..
        }]
    ));
    let partial = error.partial.unwrap();
    assert!(summary_to_value(&partial).is_err());
    assert_eq!(partial.results[0].output, Some(7));
    assert!(output.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn reporter_failure_preserves_recorded_results_and_does_not_poison_shutdown() {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let server = MockServer::start().await;
    for (endpoint, body) in [
        (
            "/api/apikey/login",
            json!({"org_info":[{"id":"org","name":"org"}]}),
        ),
        (
            "/api/experiment/register",
            json!({"project":{"id":"project","name":"project"},"experiment":{"id":"experiment","name":"experiment"}}),
        ),
        ("/logs3", json!({})),
    ] {
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .runtime(braintrust_sdk_rust::current_runtime())
        .api_key("key")
        .app_url(server.uri())
        .api_url(server.uri())
        .default_project("project")
        .blocking_login(true)
        .build()
        .await
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let completion = run(
        EvalRequest::recorded(options(calls.clone()), client.clone()).with_reporter(
            FailedReporter {
                stage: ReportStage::Started,
                attempts: Arc::new(AtomicUsize::new(0)),
            },
        ),
    );
    let shutdown = client.shutdown(Duration::from_secs(5));
    let (result, shutdown) = tokio::join!(completion, shutdown);
    shutdown.unwrap();
    let error = result.unwrap_err();
    assert!(matches!(
        error.failures.as_slice(),
        [EvalRunFailure::Reporting { .. }]
    ));
    let partial = error.partial.unwrap();
    assert_eq!(partial.recording.unwrap().experiment_id, "experiment");
    assert_eq!(partial.successful_cases, 4);
    let requests = server.received_requests().await.unwrap();
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
    for case in partial.results {
        assert!(rows
            .iter()
            .any(|r| r["span_id"] == *case.span_id.as_ref().unwrap()
                && r["metrics"]["end"].is_number()));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

// A subprocess exercises actual environment parsing and stdout without mutating
// the test runner's process-wide environment or racing other tests.
#[test]
fn cli_output_child() {
    let Ok(mode) = std::env::var("BT_REPORTING_CHILD") else {
        return;
    };
    futures::executor::block_on(async {
        let mut runner = BtEvalRunner::from_env().await.unwrap();
        assert!(runner.no_send_logs());
        let calls = Arc::new(AtomicUsize::new(0));
        for name in ["excluded", "selected"] {
            let mut opts = options(calls.clone());
            opts.name = name.into();
            opts.quiet = false;
            // No recording span is provided in local CLI mode.
            opts.definition = EvalDefinition::new(
                Box::new(task({
                    let calls = calls.clone();
                    move |input: &usize, hooks| {
                        assert!(hooks.span.is_none());
                        calls.fetch_add(1, Ordering::SeqCst);
                        Box::pin(async move { Ok(*input) })
                    }
                })),
                vec![],
            )
            .unwrap();
            runner.eval("project", opts).await.unwrap();
        }
        assert!(runner.finish().await.unwrap());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            if mode == "list" { 0 } else { 4 }
        );
    });
}
#[test]
fn cli_local_jsonl_and_list_modes_need_no_credentials_and_keep_stdout_clean() {
    for mode in ["jsonl", "no-send", "list"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli_output_child", "--nocapture"])
            .env("BT_REPORTING_CHILD", mode)
            .env("BT_EVAL_LOCAL", if mode == "no-send" { "0" } else { "1" })
            .env(
                "BT_EVAL_NO_SEND_LOGS",
                if mode == "no-send" { "1" } else { "0" },
            )
            .env("BT_EVAL_LIST", if mode == "list" { "1" } else { "0" })
            .env("BT_EVAL_JSONL", "1")
            .env("BT_EVAL_TERMINATE_ON_FAILURE", "0")
            .env(
                "BT_EVAL_FILTER_PARSED",
                r#"[{"path":["name"],"pattern":"^selected$"}]"#,
            )
            .env_remove("BT_EVAL_SSE_SOCK")
            .env_remove("BT_EVAL_SSE_ADDR")
            .env_remove("BRAINTRUST_API_KEY")
            .env("BRAINTRUST_APP_URL", "http://127.0.0.1:9")
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!stdout.contains("Running evaluation:"));
        assert!(!stdout.contains("Case 1:"));
        if mode != "list" {
            let lines: Vec<_> = stdout
                .lines()
                .filter(|line| line.starts_with('{'))
                .collect();
            assert_eq!(lines.len(), 1, "{stdout}");
            let summary: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
            assert_eq!(summary["name"], "selected");
            assert_eq!(summary["total_cases"], 4);
            assert!(summary.get("recording").is_none());
        } else {
            assert!(stdout.lines().any(|line| line == "selected"), "{stdout}");
            assert!(!stdout.contains("excluded"));
            assert!(!stdout.contains('{'));
        }
    }
}

#[test]
fn source_and_reporting_failures_both_survive_in_partial_result() {
    let mut opts = options(Arc::new(AtomicUsize::new(0)));
    opts.source = Box::new(IterCaseSource::new(
        vec![
            Ok(Case {
                input: 3,
                ..Default::default()
            }),
            Err(anyhow::anyhow!("source failed")),
        ]
        .into_iter(),
    ));
    let completion = run(EvalRequest::local(opts).with_reporter(FailedReporter {
        stage: ReportStage::Finished,
        attempts: Arc::new(AtomicUsize::new(0)),
    }));
    let error = futures::executor::block_on(async { completion.await }).unwrap_err();
    assert!(matches!(
        error.failures.as_slice(),
        [
            EvalRunFailure::Source { case_index: 1, .. },
            EvalRunFailure::Reporting {
                stage: ReportStage::Finished,
                ..
            }
        ]
    ));
    assert_eq!(error.partial.unwrap().results[0].output, Some(6));
}
#[test]
fn dropping_the_observer_does_not_cancel_run_owned_reporting() {
    struct Notice(Option<std::sync::mpsc::Sender<usize>>);
    impl EvalReporter<usize, usize> for Notice {
        fn finished(&mut self, summary: &EvalSummary<usize, usize>) -> Result<(), ReportError> {
            self.0.take().unwrap().send(summary.total_cases).unwrap();
            Ok(())
        }
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let token = run(EvalRequest::local(options(Arc::new(AtomicUsize::new(0))))
        .with_reporter(Notice(Some(sender))));
    assert!(token.is_ok());
    drop(token);
    assert_eq!(receiver.recv_timeout(Duration::from_secs(5)).unwrap(), 4);
}
#[test]
fn jsonl_write_and_flush_failures_remain_io_errors_with_computed_results() {
    struct Broken {
        fail_flush: bool,
    }
    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_flush {
                Ok(bytes.len())
            } else {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }
    for fail_flush in [false, true] {
        let token = run(EvalRequest::local(options(Arc::new(AtomicUsize::new(0))))
            .with_reporter(JsonlReporter::new(Broken { fail_flush })));
        let error = futures::executor::block_on(async { token.await }).unwrap_err();
        assert!(matches!(
            error.failures.as_slice(),
            [EvalRunFailure::Reporting {
                stage: ReportStage::Finished,
                source: ReportError::Io(_),
                ..
            }]
        ));
        assert_eq!(error.partial.unwrap().successful_cases, 4);
    }
}
