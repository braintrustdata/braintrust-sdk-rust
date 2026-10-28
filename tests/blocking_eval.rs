use braintrust_sdk_rust::eval::*;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

struct Input(usize);
struct Output(usize);
fn options(
    task: Box<dyn Task<Input, Output>>,
    scorers: Vec<Box<dyn Scorer<Input, Output>>>,
) -> EvalOpts<Input, Output> {
    EvalOpts::builder()
        .name("blocking".into())
        .source(
            vec![Case {
                input: Input(3),
                expected: Some(Output(6)),
                metadata: None,
                tags: None,
                id: None,
                xact_id: None,
                created: None,
            }]
            .into(),
        )
        .definition(EvalDefinition::new(task, scorers).unwrap())
        .quiet(true)
        .runtime(tokio::runtime::Handle::current())
        .build()
}

#[tokio::test(flavor = "current_thread")]
async fn synchronous_task_uses_selected_blocking_pool_and_preserves_shared_hooks_without_clones() {
    let worker = std::thread::current().id();
    let selected = tokio::runtime::Handle::current().id();
    let entered = Arc::new(tokio::sync::Notify::new());
    let started = entered.clone();
    let (release, waiting) = std::sync::mpsc::channel();
    let waiting = Mutex::new(waiting);
    let task = SyncFnTask::new(move |input: &Input, hooks: &TaskHooks<'_, Input, Output>| {
        assert_ne!(std::thread::current().id(), worker);
        assert_eq!(tokio::runtime::Handle::current().id(), selected);
        hooks.add_metadata("blocking", serde_json::json!(true));
        hooks.add_tag("from-blocking");
        started.notify_one();
        waiting
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))?;
        Ok(Output(input.0 + hooks.expected.unwrap().0))
    });
    // An async case on the same single-thread executor must make progress while
    // the synchronous task is blocked; it releases the callback through a channel.
    let release = tokio::spawn(async move {
        entered.notified().await;
        release.send(()).unwrap();
    });
    let summary = run(EvalRequest::local(options(Box::new(task), vec![])))
        .await
        .unwrap();
    release.await.unwrap();
    assert!(summary.results[0].errors.is_empty());
    assert_eq!(summary.results[0].output.as_ref().unwrap().0, 9);
    assert_eq!(
        summary.results[0].metadata.as_ref().unwrap()["blocking"],
        true
    );
    assert_eq!(
        summary.results[0].tags.as_ref().unwrap(),
        &["from-blocking"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn synchronous_scorers_run_concurrently_and_keep_definition_order_without_clones() {
    let worker = std::thread::current().id();
    let selected = tokio::runtime::Handle::current().id();
    let (entered, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
    let mut releases = Vec::new();
    let mut scorers: Vec<Box<dyn Scorer<Input, Output>>> = Vec::new();
    for name in ["first", "second"] {
        let entered = entered.clone();
        let (release, waiting) = std::sync::mpsc::channel();
        releases.push(release);
        let waiting = Mutex::new(waiting);
        scorers.push(Box::new(SyncFnScorer::new(
            name,
            move |args: ScorerArgs<'_, Input, Output>| {
                assert_ne!(std::thread::current().id(), worker);
                assert_eq!(tokio::runtime::Handle::current().id(), selected);
                assert_eq!(args.input.0, 3);
                assert_eq!(args.output.0, 6);
                assert_eq!(args.expected.unwrap().0, 6);
                entered.send(name).unwrap();
                waiting
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))?;
                Ok(vec![Score::new(name, 1.0)])
            },
        )));
    }
    let task = SimpleFnTask::new(|input: &Input| Ok(Output(input.0 * 2)));
    let token = run(EvalRequest::local(options(Box::new(task), scorers)));
    let mut names = vec![
        arrivals.recv().await.unwrap(),
        arrivals.recv().await.unwrap(),
    ];
    names.sort();
    assert_eq!(names, ["first", "second"]);
    for release in releases.into_iter().rev() {
        release.send(()).unwrap();
    }
    let summary = token.await.unwrap();
    assert!(summary.results[0].errors.is_empty());
    assert_eq!(
        summary.results[0]
            .scores
            .iter()
            .map(|score| score.name.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
}

#[tokio::test]
async fn synchronous_panics_become_case_errors_and_later_cases_continue() {
    let opts = EvalOpts::builder()
        .name("panic".into())
        .source(
            (0..2)
                .map(|input| Case {
                    input,
                    ..Default::default()
                })
                .collect::<Vec<Case<usize, usize>>>()
                .into(),
        )
        .definition(
            EvalDefinition::new(
                Box::new(SimpleFnTask::new(|input: &usize| {
                    if *input == 0 {
                        panic!("synchronous application panic");
                    }
                    Ok(*input)
                })),
                vec![],
            )
            .unwrap(),
        )
        .quiet(true)
        .build();
    let summary = run(EvalRequest::local(opts)).await.unwrap();
    assert_eq!(summary.failed_cases, 1);
    assert_eq!(summary.successful_cases, 1);
    let [CaseError::Task { source }] = summary.results[0].errors.as_slice() else {
        panic!("expected task error")
    };
    assert!(source
        .downcast_ref::<tokio::task::JoinError>()
        .unwrap()
        .is_panic());
    assert_eq!(summary.results[1].output, Some(1));
}
