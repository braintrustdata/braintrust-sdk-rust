#[path = "support/eval_server.rs"]
mod eval_server;
use braintrust_sdk_rust::{
    eval::*, BraintrustClient, EnqueueError, ExecutionError, SpanLog, WriteError,
};
use std::{
    future::IntoFuture,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use wiremock::MockServer;

fn options<I: Send + Sync + 'static, O: Send + Sync + 'static>(
    definition: EvalDefinition<I, O>,
    cases: Vec<Case<I, O>>,
) -> EvalOpts<I, O> {
    EvalOpts::builder()
        .name("eager".into())
        .definition(definition)
        .source(cases.into())
        .quiet(true)
        .build()
}
fn numeric() -> EvalOpts<usize, usize> {
    options(
        EvalDefinition::new(
            Box::new(SimpleFnTask::new(|input: &usize| Ok(input * 2))),
            vec![],
        )
        .unwrap(),
        vec![Case::builder().input(3).build()],
    )
}

#[test]
fn local_runs_from_a_plain_thread_without_serialization_clone_or_debug() {
    struct Input(usize);
    struct Output(usize);
    let definition = EvalDefinition::new(
        Box::new(SimpleFnTask::new(|input: &Input| Ok(Output(input.0 * 2)))),
        vec![],
    )
    .unwrap();
    let token = run(options(
        definition,
        vec![Case::builder().input(Input(3)).build()],
    ));
    assert!(token.is_ok());
    let result = futures::executor::block_on(token.into_future()).unwrap();
    assert_eq!(result.results()[0].output().unwrap().0, 6);
    assert!(result.results()[0].span_id().is_none());
}

#[test]
fn dropping_the_observer_keeps_the_source_and_callbacks_running() {
    let (done, observed) = std::sync::mpsc::channel();
    let definition = EvalDefinition::new(
        Box::new(SimpleFnTask::new(move |input: &usize| {
            done.send(*input).unwrap();
            Ok(*input)
        })),
        vec![],
    )
    .unwrap();
    let token = run(options(definition, vec![Case::builder().input(7).build()]));
    assert!(token.is_ok());
    drop(token);
    assert_eq!(observed.recv_timeout(Duration::from_secs(3)).unwrap(), 7);
}

#[tokio::test]
async fn local_runtime_selection_is_explicit_and_never_uses_ambient_by_default() {
    let ambient = tokio::runtime::Handle::current().id();
    let definition = EvalDefinition::new(
        Box::new(SimpleFnTask::new(|_: &usize| {
            Ok(tokio::runtime::Handle::current().id())
        })),
        vec![],
    )
    .unwrap();
    for explicit in [false, true] {
        let opts = EvalOpts::builder()
            .name("eager".into())
            .definition(definition.clone())
            .source(vec![Case::builder().input(0).build()].into())
            .quiet(true)
            .maybe_runtime(explicit.then(tokio::runtime::Handle::current))
            .build();
        let summary = run(opts).await.unwrap();
        assert_eq!(
            summary.results()[0].output().copied() == Some(ambient),
            explicit
        );
    }
}

#[test]
fn external_runtime_loss_before_first_poll_reports_worker_loss_without_fallback() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
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
        .name("eager".into())
        .definition(definition)
        .source(vec![Case::builder().input(0).build()].into())
        .quiet(true)
        .maybe_runtime(Some(runtime.handle().clone()))
        .build();
    let token = run(opts);
    assert!(token.is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(runtime);
    let error = futures::executor::block_on(token.into_future()).unwrap_err();
    assert!(matches!(
        error.failures(),
        [EvalRunFailure::Execution(ExecutionError::WorkerLost)]
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn invalid_options_return_an_immediate_config_error() {
    let opts = EvalOpts::builder()
        .name("eager".into())
        .definition(
            EvalDefinition::new(
                Box::new(SimpleFnTask::new(|input: &usize| Ok(input * 2))),
                vec![],
            )
            .unwrap(),
        )
        .source(vec![Case::builder().input(3).build()].into())
        .quiet(true)
        .max_concurrency(0)
        .build();
    let token = run(opts);
    assert!(token.is_err());
    let error = futures::executor::block_on(token.into_future()).unwrap_err();
    assert!(matches!(
        error.failures(),
        [EvalRunFailure::Config(EvalConfigError::ZeroConcurrency)]
    ));
    assert!(error.partial().is_none());
}

async fn client(server: &MockServer) -> BraintrustClient {
    eval_server::mount(server, true).await;
    BraintrustClient::builder()
        .runtime(braintrust_sdk_rust::current_runtime())
        .app_url(server.uri())
        .api_url(server.uri())
        .api_key("token")
        .default_project("project")
        .blocking_login(true)
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn recorded_run_admitted_before_shutdown_finishes_writes_and_revokes_escaped_spans() {
    let server = MockServer::start().await;
    let client = client(&server).await;
    let escaped = Arc::new(Mutex::new(None));
    let definition = EvalDefinition::new(
        Box::new(task({
            let escaped = escaped.clone();
            move |input: &usize, hooks| {
                let escaped = escaped.clone();
                Box::pin(async move {
                    let child = hooks
                        .span()
                        .unwrap()
                        .child_builder()
                        .unwrap()
                        .name("callback-child")
                        .build();
                    child
                        .log(SpanLog::builder().input(serde_json::json!(input)).build())
                        .await
                        .unwrap();
                    *escaped.lock().unwrap() = Some(child);
                    Ok(*input)
                })
            }
        })),
        vec![],
    )
    .unwrap();
    let token = client
        .clone()
        .eval(options(definition, vec![Case::builder().input(7).build()]));
    assert!(token.is_ok());
    let shutdown = client.shutdown(Duration::from_secs(5));
    let rejected = client.clone().eval(numeric());
    assert!(rejected.is_err());
    assert!(matches!(
        rejected.await.unwrap_err().failures(),
        [EvalRunFailure::Admission(EnqueueError::Closed)]
    ));
    let (result, shutdown) = tokio::join!(token, shutdown);
    let result = result.unwrap();
    shutdown.unwrap();
    assert_eq!(result.results()[0].output().copied(), Some(7));
    assert!(result.results()[0].span_id().is_some());
    let escaped = escaped.lock().unwrap().take().unwrap();
    assert!(matches!(
        escaped.end().await,
        Err(WriteError::Admission(EnqueueError::Closed))
    ));
}

#[tokio::test]
async fn recorded_runtime_override_is_rejected_and_dropped_observer_still_drains() {
    let server = MockServer::start().await;
    let client = client(&server).await;
    let opts = EvalOpts::builder()
        .name("eager".into())
        .definition(
            EvalDefinition::new(
                Box::new(SimpleFnTask::new(|input: &usize| Ok(input * 2))),
                vec![],
            )
            .unwrap(),
        )
        .source(vec![Case::builder().input(3).build()].into())
        .quiet(true)
        .maybe_runtime(braintrust_sdk_rust::current_runtime())
        .build();
    let rejected = client.clone().eval(opts);
    assert!(rejected.is_err());
    assert!(matches!(
        rejected.await.unwrap_err().failures(),
        [EvalRunFailure::Config(EvalConfigError::RecordedRuntime)]
    ));
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
    drop(
        client
            .clone()
            .eval(options(definition, vec![Case::builder().input(1).build()])),
    );
    client.shutdown(Duration::from_secs(5)).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn callback_panic_retires_recorded_run_and_revokes_escaped_permission() {
    let server = MockServer::start().await;
    let client = client(&server).await;
    let escaped = Arc::new(Mutex::new(None));
    let definition: EvalDefinition<usize, usize> = EvalDefinition::new(
        Box::new(task({
            let escaped = escaped.clone();
            move |_: &usize, hooks| {
                *escaped.lock().unwrap() =
                    Some(hooks.span().unwrap().child_builder().unwrap().build());
                Box::pin(async { panic!("application callback panic") })
            }
        })),
        vec![],
    )
    .unwrap();
    let token = client
        .clone()
        .eval(options(definition, vec![Case::builder().input(0).build()]));
    let error = token.await.unwrap_err();
    assert!(matches!(
        error.failures(),
        [EvalRunFailure::Execution(ExecutionError::WorkerLost)]
    ));
    client.shutdown(Duration::from_secs(5)).await.unwrap();
    let escaped = escaped.lock().unwrap().take().unwrap();
    assert!(matches!(
        escaped.end().await,
        Err(WriteError::Admission(EnqueueError::Closed))
    ));
}

#[tokio::test]
async fn shutdown_deadline_cancels_pending_run_and_revokes_escaped_permission() {
    let server = MockServer::start().await;
    let client = client(&server).await;
    let escaped = Arc::new(Mutex::new(None));
    let entered = Arc::new(tokio::sync::Notify::new());
    let definition: EvalDefinition<usize, usize> = EvalDefinition::new(
        Box::new(task({
            let escaped = escaped.clone();
            let entered = entered.clone();
            move |_: &usize, hooks| {
                *escaped.lock().unwrap() =
                    Some(hooks.span().unwrap().child_builder().unwrap().build());
                entered.notify_one();
                Box::pin(std::future::pending())
            }
        })),
        vec![],
    )
    .unwrap();
    let token = client
        .clone()
        .eval(options(definition, vec![Case::builder().input(0).build()]));
    entered.notified().await;
    assert!(matches!(
        client.shutdown(Duration::from_millis(20)).await,
        Err(braintrust_sdk_rust::ShutdownError::Execution(
            ExecutionError::TimedOut
        ))
    ));
    let error = token.await.unwrap_err();
    assert!(matches!(
        error.failures(),
        [EvalRunFailure::Execution(ExecutionError::TimedOut)]
    ));
    let escaped = escaped.lock().unwrap().take().unwrap();
    assert!(matches!(
        escaped.end().await,
        Err(WriteError::Admission(EnqueueError::Closed))
    ));
}

#[tokio::test]
async fn running_blocking_callback_keeps_owned_data_after_timeout_but_loses_write_permission() {
    #[derive(serde::Serialize)]
    struct OwnedInput {
        value: usize,
        #[serde(skip)]
        drops: Arc<AtomicUsize>,
        #[serde(skip)]
        dropped: Arc<tokio::sync::Notify>,
    }
    impl Drop for OwnedInput {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
            self.dropped.notify_one();
        }
    }
    let server = MockServer::start().await;
    let client = client(&server).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let drops = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(tokio::sync::Notify::new());
    let (release, waiting) = std::sync::mpsc::channel();
    let waiting = Mutex::new(waiting);
    let (report, observed) = tokio::sync::oneshot::channel();
    let report = Mutex::new(Some(report));
    let callback = SyncFnTask::new({
        let entered = entered.clone();
        move |input: &OwnedInput, hooks: &TaskHooks<'_, OwnedInput, usize>| {
            entered.notify_one();
            waiting
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))?;
            let rejected = hooks.span().unwrap().log(SpanLog::default()).is_err();
            report
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send((input.value, rejected))
                .unwrap();
            Ok(input.value)
        }
    });
    let opts = options(
        EvalDefinition::new(Box::new(callback), vec![]).unwrap(),
        vec![Case::builder()
            .input(OwnedInput {
                value: 17,
                drops: drops.clone(),
                dropped: dropped.clone(),
            })
            .build()],
    );
    let token = client.clone().eval(opts);
    entered.notified().await;
    assert!(client.shutdown(Duration::from_millis(20)).await.is_err());
    assert!(matches!(
        token.await.err().expect("run must time out").failures(),
        [EvalRunFailure::Execution(ExecutionError::TimedOut)]
    ));
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "running callback still owns the input"
    );
    release.send(()).unwrap();
    assert_eq!(observed.await.unwrap(), (17, true));
    dropped.notified().await;
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn synchronous_callback_can_wait_for_recording_without_blocking_delivery() {
    let server = MockServer::start().await;
    let client = client(&server).await;
    let worker = std::thread::current().id();
    let callback = SyncFnTask::new(move |input: &usize, hooks: &TaskHooks<'_, usize, usize>| {
        assert_ne!(std::thread::current().id(), worker);
        futures::executor::block_on(
            hooks
                .span()
                .unwrap()
                .log(
                    SpanLog::builder()
                        .metadata(
                            serde_json::json!({"sync_write": true})
                                .as_object()
                                .unwrap()
                                .clone(),
                        )
                        .build(),
                )
                .into_future(),
        )?;
        Ok(*input)
    });
    let token = client.clone().eval(options(
        EvalDefinition::new(Box::new(callback), vec![]).unwrap(),
        vec![Case::builder().input(9).build()],
    ));
    let shutdown = client.shutdown(Duration::from_secs(5));
    let (result, shutdown) = tokio::join!(token, shutdown);
    assert_eq!(result.unwrap().results()[0].output().copied(), Some(9));
    shutdown.unwrap();
}

#[test]
fn local_run_context_uses_case_overrides_without_a_recording_target() {
    let definition = EvalDefinition::new(
        Box::new(SyncFnTask::new(
            |input: &usize, hooks: &TaskHooks<'_, usize, usize>| {
                assert_eq!(hooks.get_metadata().unwrap()["run"], true);
                assert_eq!(hooks.get_metadata().unwrap()["collision"], "case");
                hooks.add_metadata("task", serde_json::json!(true));
                Ok(*input)
            },
        )),
        vec![],
    )
    .unwrap();
    let opts = EvalOpts::builder()
        .name("eager".into())
        .definition(definition)
        .source(
            vec![Case::builder()
                .input(1)
                .metadata(
                    serde_json::json!({"collision":"case"})
                        .as_object()
                        .unwrap()
                        .clone(),
                )
                .tags(vec!["shared".into(), "case".into()])
                .build()]
            .into(),
        )
        .quiet(true)
        .metadata(
            serde_json::json!({"run":true, "collision":"run"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .tags(vec!["run".into(), "shared".into()])
        .build();
    let summary = futures::executor::block_on(run(opts).into_future()).unwrap();
    assert!(summary.recording().is_none());
    assert_eq!(summary.results()[0].metadata().unwrap()["task"], true);
    assert_eq!(
        summary.results()[0].tags().unwrap(),
        &["run", "shared", "case"]
    );
}

#[test]
fn options_reporter_overrides_quiet_and_observes_local_results() {
    struct Reporter(Arc<Mutex<Vec<String>>>);
    // Deliberately no Serialize or Clone on local output.
    struct Output(usize);
    impl EvalReporter<usize, Output> for Reporter {
        fn started(&mut self, name: &str) -> Result<(), ReportError> {
            self.0.lock().unwrap().push(name.into());
            Ok(())
        }
        fn case_finished(
            &mut self,
            index: usize,
            result: &EvalResult<usize, Output>,
        ) -> Result<(), ReportError> {
            self.0
                .lock()
                .unwrap()
                .push(format!("{index}:{}", result.output().unwrap().0));
            Ok(())
        }
        fn finished(&mut self, summary: &EvalSummary<usize, Output>) -> Result<(), ReportError> {
            self.0
                .lock()
                .unwrap()
                .push(format!("done:{}", summary.total_cases()));
            Ok(())
        }
    }
    let events = Arc::new(Mutex::new(vec![]));
    let opts = options(
        EvalDefinition::new(
            Box::new(SimpleFnTask::new(|input: &usize| Ok(Output(input * 2)))),
            vec![],
        )
        .unwrap(),
        vec![Case::builder().input(3).build()],
    )
    .with_reporter(Reporter(events.clone()));
    futures::executor::block_on(run(opts).into_future()).unwrap();
    assert_eq!(*events.lock().unwrap(), ["eager", "0:6", "done:1"]);
}
