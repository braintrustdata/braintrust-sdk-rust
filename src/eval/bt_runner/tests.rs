use std::sync::Mutex;

use super::*;
use crate::eval::EvalRequest;

#[test]
fn test_env_flag() {
    assert!(!env_flag("BT_TEST_NONEXISTENT_FLAG_XYZ"));
}
#[test]
fn test_parse_filters_empty() {
    assert!(parse_filters(None).unwrap().is_empty());
    assert!(parse_filters(Some("")).unwrap().is_empty());
}
#[test]
fn test_parse_filters_valid() {
    let filters = parse_filters(Some(r#"[{"path":["name"],"pattern":"foo.*"}]"#)).unwrap();
    assert_eq!(filters.len(), 1);
    assert!(filters[0].matches("foobar"));
    assert!(!filters[0].matches("bar"));
}
#[test]
fn test_parse_filters_invalid_json() {
    assert!(matches!(
        parse_filters(Some("not-json")),
        Err(BtInitError::Filters(_))
    ));
}

#[test]
fn test_eval_filter_matches_by_name() {
    let filter = EvalFilter {
        path: vec!["name".to_string()],
        pattern: regex::Regex::new("^my-.*").unwrap(),
    };
    assert!(filter.matches("my-eval"));
    assert!(!filter.matches("your-eval"));
}

#[test]
fn test_eval_filter_empty_path_matches_name() {
    let filter = EvalFilter {
        path: vec![],
        pattern: regex::Regex::new("foo").unwrap(),
    };
    assert!(filter.matches("foo-eval"));
    assert!(!filter.matches("bar-eval"));
}
#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);
impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn local_config(terminate_on_failure: bool) -> RunnerConfig {
    RunnerConfig {
        jsonl: true,
        list: false,
        terminate_on_failure,
        filters: vec![],
        no_send_logs: true,
        endpoint: None,
    }
}
fn local_reported_runner(config: RunnerConfig, sse: Arc<SseWriter>) -> BtEvalRunner {
    BtEvalRunner::assemble(
        config,
        Some(sse),
        None,
        Some(ExecutionBinding::new(None).unwrap()),
    )
}
fn local_options(fail: bool) -> EvalOpts<usize, usize> {
    super::super::EvalOpts::builder()
        .name("cli".into())
        .quiet(false)
        .source(
            vec![super::super::Case {
                input: 1,
                ..Default::default()
            }]
            .into(),
        )
        .definition(
            super::super::EvalDefinition::new(
                Box::new(super::super::SimpleFnTask::new(move |input: &usize| {
                    if fail {
                        anyhow::bail!("case failed")
                    } else {
                        Ok(*input)
                    }
                })),
                vec![],
            )
            .unwrap(),
        )
        .build()
}
#[tokio::test]
async fn sse_protocol_is_the_only_reporter_and_case_failures_update_cli_status() {
    let output = Buffer::default();
    let sse = Arc::new(SseWriter {
        writer: Mutex::new(Box::new(output.clone())),
    });
    let mut runner = local_reported_runner(local_config(false), sse);
    assert!(runner.client.is_none());
    runner.eval("project", local_options(false)).await.unwrap();
    runner.eval("project", local_options(true)).await.unwrap();
    assert!(!runner.all_passed());
    assert!(!runner.finish().await.unwrap());
    let bytes = output.0.lock().unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    let events: Vec<_> = text.split("\n\n").filter(|s| !s.is_empty()).collect();
    assert_eq!(events.len(), 4);
    assert!(events[0].starts_with("event: start\ndata: "));
    assert!(events[1].starts_with("event: summary\ndata: "));
    assert!(events[2].starts_with("event: start\ndata: "));
    let summary: Value =
        serde_json::from_str(events[3].strip_prefix("event: summary\ndata: ").unwrap()).unwrap();
    assert_eq!(summary["failed_cases"], 1);
}
#[tokio::test]
async fn terminate_on_failure_returns_an_error_and_retains_failed_session_status() {
    let sse = Arc::new(SseWriter {
        writer: Mutex::new(Box::new(Buffer::default())),
    });
    let mut runner = local_reported_runner(local_config(true), sse);
    assert!(runner.eval("project", local_options(true)).await.is_err());
    assert!(!runner.all_passed());
    assert!(!runner.finish().await.unwrap());
}
#[tokio::test]
async fn sse_write_failure_is_a_report_failure_without_losing_computation() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "closed socket",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let sse = Arc::new(SseWriter {
        writer: Mutex::new(Box::new(Broken)),
    });
    let result = super::super::run(
        EvalRequest::local(local_options(false)).with_reporter(SseReporter { sse: sse.clone() }),
    )
    .await;
    let error = result.unwrap_err();
    assert!(matches!(
        error.failures.as_slice(),
        [super::super::EvalRunFailure::Reporting {
            stage: super::super::ReportStage::Started,
            source: ReportError::Io(_),
            ..
        }]
    ));
    assert_eq!(error.partial.unwrap().successful_cases, 1);
}
fn quiet_options(input: usize) -> EvalOpts<usize, usize> {
    let mut opts = local_options(false);
    opts.quiet = true;
    opts.source = vec![super::super::Case {
        input,
        ..Default::default()
    }]
    .into();
    opts
}
fn runner_on(binding: ExecutionBinding, terminate: bool) -> BtEvalRunner {
    let mut config = local_config(terminate);
    config.jsonl = false;
    BtEvalRunner {
        config,
        sse: None,
        client: None,
        binding: Some(binding),
        session: Session::new(terminate),
        registered_names: Vec::new(),
    }
}
#[tokio::test]
async fn eager_runs_keep_call_order_and_finish_joins_discarded_observers() {
    use crate::eval::{task, EvalDefinition};
    let mut runner = runner_on(ExecutionBinding::new(None).unwrap(), false);
    let order = Arc::new(Mutex::new(Vec::new()));
    let entered = Arc::new(tokio::sync::Notify::new());
    let (release, wait) = futures::channel::oneshot::channel();
    let wait = Mutex::new(Some(wait));
    let mut first = quiet_options(1);
    first.definition = EvalDefinition::new(
        Box::new(task({
            let entered = entered.clone();
            let order = order.clone();
            move |input: &usize, _| {
                let wait = wait.lock().unwrap().take().unwrap();
                let entered = entered.clone();
                let order = order.clone();
                Box::pin(async move {
                    entered.notify_one();
                    wait.await?;
                    order.lock().unwrap().push(*input);
                    Ok(*input)
                })
            }
        })),
        vec![],
    )
    .unwrap();
    let mut second = quiet_options(2);
    second.definition = EvalDefinition::new(
        Box::new(super::super::SimpleFnTask::new({
            let order = order.clone();
            move |input: &usize| {
                order.lock().unwrap().push(*input);
                Ok(*input)
            }
        })),
        vec![],
    )
    .unwrap();
    let first = runner.eval("project", first);
    let second = runner.eval("project", second);
    assert!(first.is_ok());
    assert!(second.is_ok());
    drop(first);
    drop(second);
    let finished = runner.finish();
    assert!(finished.is_ok());
    entered.notified().await;
    assert!(order.lock().unwrap().is_empty());
    release.send(()).unwrap();
    assert!(finished.await.unwrap());
    assert_eq!(*order.lock().unwrap(), [1, 2]);
}
#[tokio::test]
async fn terminate_on_failure_skips_already_admitted_successors_without_reading_sources() {
    use crate::eval::{task, EvalDefinition, IterCaseSource};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut runner = runner_on(
        ExecutionBinding::external(tokio::runtime::Handle::current()),
        true,
    );
    let (release, wait) = futures::channel::oneshot::channel();
    let wait = Mutex::new(Some(wait));
    let mut first = quiet_options(1);
    first.definition = EvalDefinition::new(
        Box::new(task(move |_: &usize, _| {
            let wait = wait.lock().unwrap().take().unwrap();
            Box::pin(async move {
                wait.await?;
                anyhow::bail!("case failed")
            })
        })),
        vec![],
    )
    .unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let mut second = quiet_options(2);
    second.source = Box::new(IterCaseSource::new({
        let reads = reads.clone();
        (0..1).map(move |input| {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok(super::super::Case {
                input,
                ..Default::default()
            })
        })
    }));
    let first = runner.eval("project", first);
    let second = runner.eval("project", second);
    assert!(first.is_ok());
    assert!(second.is_ok());
    release.send(()).unwrap();
    let BtEvalError::CaseFailures { summary } = first.await.unwrap_err() else {
        panic!("case failure must retain summary")
    };
    assert_eq!(summary.failed_cases, 1);
    assert!(matches!(second.await, Err(BtEvalError::Stopped)));
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let later = runner.eval("project", quiet_options(3));
    assert!(later.is_err());
    assert!(matches!(later.await, Err(BtEvalError::Stopped)));
    assert!(!runner.finish().await.unwrap());
}
#[tokio::test]
async fn full_cli_admission_uses_one_slot_per_run_and_finish_has_control_capacity() {
    use crate::{
        eval::{task, EvalDefinition},
        internal::response_tasks::MAX_RESPONSE_TASKS,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    let binding = ExecutionBinding::external(tokio::runtime::Handle::current());
    let mut runner = runner_on(binding.clone(), false);
    let count = Arc::new(AtomicUsize::new(0));
    let (release, wait) = futures::channel::oneshot::channel();
    let wait = Mutex::new(Some(wait));
    let mut first = quiet_options(0);
    first.definition = EvalDefinition::new(
        Box::new(task({
            let count = count.clone();
            move |input: &usize, _| {
                let wait = wait.lock().unwrap().take().unwrap();
                let count = count.clone();
                Box::pin(async move {
                    wait.await?;
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(*input)
                })
            }
        })),
        vec![],
    )
    .unwrap();
    let first = runner.eval("project", first);
    assert!(first.is_ok());
    drop(first);
    for index in 1..MAX_RESPONSE_TASKS {
        let mut opts = quiet_options(index);
        opts.definition = EvalDefinition::new(
            Box::new(super::super::SimpleFnTask::new({
                let count = count.clone();
                move |input: &usize| {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(*input)
                }
            })),
            vec![],
        )
        .unwrap();
        let token = runner.eval("project", opts);
        assert!(token.is_ok());
        drop(token);
    }
    assert_eq!(binding.outstanding_responses(), MAX_RESPONSE_TASKS);
    let rejected = runner.eval("project", quiet_options(99));
    assert!(rejected.is_err());
    assert!(
        matches!(rejected.await, Err(BtEvalError::Run(error)) if matches!(error.failures.as_slice(), [super::super::EvalRunFailure::Admission(crate::EnqueueError::Full)]))
    );
    let finish = runner.finish();
    assert!(finish.is_ok());
    release.send(()).unwrap();
    assert!(!finish.await.unwrap());
    assert_eq!(count.load(Ordering::SeqCst), MAX_RESPONSE_TASKS);
    assert_eq!(
        binding.outstanding_responses(),
        0,
        "finish includes response publication"
    );
}
#[test]
fn external_runtime_loss_before_first_poll_retires_cli_work_without_fallback() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut runner = runner_on(ExecutionBinding::external(runtime.handle().clone()), false);
    let status = runner.session.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let mut opts = quiet_options(1);
    opts.definition = super::super::EvalDefinition::new(
        Box::new(super::super::SimpleFnTask::new({
            let count = count.clone();
            move |input: &usize| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(*input)
            }
        })),
        vec![],
    )
    .unwrap();
    let run = runner.eval("project", opts);
    assert!(run.is_ok());
    let finish = runner.finish();
    assert!(finish.is_ok());
    drop(runtime);
    assert!(
        matches!(futures::executor::block_on(async { run.await }), Err(BtEvalError::Run(error)) if matches!(error.failures.as_slice(), [super::super::EvalRunFailure::Execution(crate::ExecutionError::WorkerLost)]))
    );
    assert!(matches!(
        futures::executor::block_on(async { finish.await }),
        Err(BtFinishError::Execution(crate::ExecutionError::WorkerLost))
    ));
    assert!(!status.all_passed());
    assert_eq!(count.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn panic_releases_the_cli_order_guard_and_obeys_termination_policy() {
    use crate::eval::{task, EvalDefinition};
    for terminate in [false, true] {
        let mut runner = runner_on(
            ExecutionBinding::external(tokio::runtime::Handle::current()),
            terminate,
        );
        let mut first = quiet_options(1);
        first.definition = EvalDefinition::new(
            Box::new(task(|_: &usize, _| {
                Box::pin(async { panic!("async callback panic") })
            })),
            vec![],
        )
        .unwrap();
        let first = runner.eval("project", first);
        let second = runner.eval("project", quiet_options(2));
        assert!(
            matches!(first.await, Err(BtEvalError::Run(error)) if matches!(error.failures.as_slice(), [super::super::EvalRunFailure::Execution(crate::ExecutionError::WorkerLost)]))
        );
        let second = second.await;
        if terminate {
            assert!(matches!(second, Err(BtEvalError::Stopped)));
        } else {
            second.unwrap();
        }
        assert!(!runner.finish().await.unwrap());
    }
}
#[tokio::test]
async fn immediate_rejections_do_not_advance_the_cli_order_tail() {
    let binding = ExecutionBinding::external(tokio::runtime::Handle::current());
    let mut runner = runner_on(binding.clone(), false);
    let tail = runner.session.tail();
    let mut opts = quiet_options(1);
    opts.max_concurrency = 0;
    let rejected = runner.eval("project", opts);
    assert!(rejected.is_err());
    assert!(
        matches!(rejected.await, Err(BtEvalError::Run(error)) if matches!(error.failures.as_slice(), [super::super::EvalRunFailure::Config(super::super::EvalConfigError::ZeroConcurrency)]))
    );
    let mut opts = quiet_options(1);
    opts.runtime = crate::current_runtime();
    let rejected = runner.eval("project", opts);
    assert!(rejected.is_err());
    assert!(matches!(rejected.await, Err(BtEvalError::RuntimeOverride)));
    assert!(tail.ptr_eq(&runner.session.tail()));
    runner.eval("project", quiet_options(2)).await.unwrap();
    assert!(!runner.finish().await.unwrap());
    assert_eq!(binding.outstanding_responses(), 0);
}
#[tokio::test]
async fn recorded_cli_runs_use_existing_admission_and_finish_does_not_close_shared_clients() {
    use std::time::Duration;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    for shutdown_first in [true, false] {
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
            .runtime(crate::current_runtime())
            .api_key("key")
            .app_url(server.uri())
            .api_url(server.uri())
            .default_project("project")
            .blocking_login(true)
            .build()
            .await
            .unwrap();
        let mut runner = runner_on(client.execution_binding(), false);
        runner.config.no_send_logs = false;
        runner.client = Some(Arc::new(client.clone()));
        let first = runner.eval("project", quiet_options(1));
        let second = runner.eval("project", quiet_options(2));
        assert!(first.is_ok());
        assert!(second.is_ok());
        assert_eq!(client.execution_binding().outstanding_responses(), 2);
        let shutdown = shutdown_first.then(|| client.shutdown(Duration::from_secs(5)));
        let finished = runner.finish();
        assert!(finished.is_ok());
        assert!(finished.await.unwrap());
        // Joining the session includes publication of every earlier token.
        use futures::FutureExt;
        use std::future::IntoFuture;
        first.into_future().now_or_never().unwrap().unwrap();
        second.into_future().now_or_never().unwrap().unwrap();
        if let Some(shutdown) = shutdown {
            shutdown.await.unwrap();
        } else {
            client.wait_for_login().await.unwrap();
            client.flush().await.unwrap();
            client.shutdown(Duration::from_secs(5)).await.unwrap();
        }
        let requests = server.received_requests().await.unwrap();
        let rows: Vec<Value> = requests
            .iter()
            .filter(|r| r.url.path() == "/logs3")
            .flat_map(|r| {
                serde_json::from_slice::<Value>(&r.body).unwrap()["rows"]
                    .as_array()
                    .unwrap()
                    .clone()
            })
            .collect();
        assert_eq!(
            rows.iter()
                .filter(|r| r["metrics"]["end"].is_number())
                .count(),
            2
        );
    }
}
#[tokio::test]
async fn source_failure_keeps_its_typed_partial_summary_at_cli_boundary() {
    use crate::eval::{Case, EvalRunFailure, IterCaseSource};
    let mut runner = runner_on(
        ExecutionBinding::external(tokio::runtime::Handle::current()),
        true,
    );
    let mut opts = quiet_options(1);
    opts.max_concurrency = 1;
    opts.source = Box::new(IterCaseSource::new(
        vec![
            Ok(Case {
                input: 1,
                ..Default::default()
            }),
            Err(anyhow::anyhow!("source failed")),
        ]
        .into_iter(),
    ));
    let token = runner.eval("project", opts);
    assert!(token.is_ok());
    let BtEvalError::Run(error) = token.await.unwrap_err() else {
        panic!("expected typed run failure")
    };
    assert!(matches!(
        error.failures.as_slice(),
        [EvalRunFailure::Source { case_index: 1, .. }]
    ));
    assert_eq!(error.partial.unwrap().successful_cases, 1);
    assert!(!runner.finish().await.unwrap());
}
#[tokio::test]
async fn finish_closes_sse_before_publication_even_when_its_observer_is_dropped() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Close {
        output: Buffer,
        closed: Arc<AtomicBool>,
        notice: Option<tokio::sync::oneshot::Sender<()>>,
    }
    impl Write for Close {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.output.write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.output.flush()
        }
    }
    impl Drop for Close {
        fn drop(&mut self) {
            self.closed.store(true, Ordering::SeqCst);
            let _ = self.notice.take().unwrap().send(());
        }
    }
    for discard in [true, false] {
        let closed = Arc::new(AtomicBool::new(false));
        let (notice, closing) = tokio::sync::oneshot::channel();
        let sse = Arc::new(SseWriter {
            writer: Mutex::new(Box::new(Close {
                output: Buffer::default(),
                closed: closed.clone(),
                notice: Some(notice),
            })),
        });
        let mut runner = local_reported_runner(local_config(false), sse);
        drop(runner.eval("project", quiet_options(1)));
        let finish = runner.finish();
        assert!(finish.is_ok());
        if discard {
            drop(finish);
            closing.await.unwrap();
        } else {
            assert!(finish.await.unwrap());
        }
        assert!(closed.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn later_issuance_failure_does_not_skip_earlier_accepted_work() {
    let mut runner = runner_on(
        ExecutionBinding::external(tokio::runtime::Handle::current()),
        true,
    );
    let earlier = runner.eval("project", quiet_options(1));
    let mut invalid = quiet_options(2);
    invalid.runtime = crate::current_runtime();
    let rejected = runner.eval("project", invalid);
    assert!(rejected.is_err());
    assert!(matches!(rejected.await, Err(BtEvalError::RuntimeOverride)));
    earlier.await.unwrap();
    assert!(matches!(
        runner.eval("project", quiet_options(3)).await,
        Err(BtEvalError::Stopped)
    ));
    assert!(!runner.finish().await.unwrap());
}

#[tokio::test]
async fn constructor_uses_one_client_response_slot_and_rejects_before_socket_work() {
    use crate::internal::response_tasks::MAX_RESPONSE_TASKS;
    let client = BraintrustClient::builder()
        .runtime(crate::current_runtime())
        .skip_login(true)
        .build()
        .await
        .unwrap();
    let binding = client.execution_binding();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    for _ in 0..MAX_RESPONSE_TASKS {
        let gate = gate.clone();
        let token = binding.response::<(), BraintrustError, _>(async move {
            gate.acquire().await.unwrap().forget();
            Ok(())
        });
        assert!(token.is_ok());
        drop(token);
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut config = local_config(false);
    config.endpoint = Some(SseEndpoint::Tcp(listener.local_addr().unwrap().to_string()));
    let rejected = BtEvalRunner::initialize(config, Some(Arc::new(client.clone())));
    assert!(rejected.is_err());
    assert!(matches!(
        rejected.await,
        Err(BtInitError::Admission(crate::EnqueueError::Full))
    ));
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    gate.add_permits(MAX_RESPONSE_TASKS);
    binding.responses_drained().await.unwrap();
    let token = BtEvalRunner::initialize(local_config(false), Some(Arc::new(client.clone())));
    assert!(token.is_ok());
    assert_eq!(binding.outstanding_responses(), 1);
    let runner = token.await.unwrap();
    assert!(runner.finish().await.unwrap());
    client
        .shutdown(std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let rejected = BtEvalRunner::initialize(local_config(false), Some(Arc::new(client.clone())));
    assert!(rejected.is_err());
    assert!(matches!(
        rejected.await,
        Err(BtInitError::Admission(crate::EnqueueError::Closed))
    ));
    let mut list = local_config(false);
    list.list = true;
    let token = BtEvalRunner::initialize(list, Some(Arc::new(client)));
    assert!(token.is_ok());
    let list = token.await.unwrap();
    assert!(list.binding.is_none());
    assert!(list.sse.is_none());
    assert!(list.finish().await.unwrap());
}
