use async_trait::async_trait;
use braintrust_sdk_rust::{
    eval::{
        scorer, task, Case, Dataset, EvalConfigError, EvalOpts, Evaluator, ExactMatch, Score,
        Scorer, ScorerArgs, Scores, SimpleFnTask, Task, TaskHooks,
    },
    BraintrustClient, BraintrustError,
};
use futures::future::BoxFuture;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

// No Clone, serde or equality implementations are needed to adapt callables.
struct Input(usize);
struct Output(usize);

fn borrowed_task<'a>(
    input: &'a Input,
    hooks: &'a TaskHooks<'a, Input, Output>,
) -> BoxFuture<'a, anyhow::Result<Output>> {
    Box::pin(async move {
        tokio::task::yield_now().await;
        Ok(Output(input.0 + hooks.expected.unwrap().0))
    })
}
fn borrowed_score<'a>(
    args: ScorerArgs<'a, Input, Output>,
) -> BoxFuture<'a, anyhow::Result<Scores>> {
    Box::pin(async move {
        tokio::task::yield_now().await;
        Ok(vec![Score::new(
            "borrowed",
            (args.output.0 - args.input.0) as f64,
        )])
    })
}

#[tokio::test]
async fn functions_and_plain_named_tuples_borrow_values_across_await() {
    let task: Box<dyn Task<Input, Output>> = Box::new(borrowed_task);
    let scorer: Box<dyn Scorer<Input, Output>> = Box::new(("function", borrowed_score));
    let input = Input(4);
    let expected = Output(3);
    let hooks = TaskHooks::local(Some(&expected));
    let output = task.run(&input, &hooks).await.unwrap();
    assert_eq!(output.0, 7);
    assert_eq!(scorer.name(), "function");
    let scores = scorer
        .score(ScorerArgs {
            input: &input,
            output: &output,
            expected: Some(&expected),
            metadata: None,
        })
        .await
        .unwrap();
    assert_eq!(scores[0].score, 3.0);
}

#[tokio::test]
async fn captured_closure_is_reusable_concurrently_without_wrappers_or_value_clones() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let gate = Arc::new(tokio::sync::Barrier::new(2));
    let task: Arc<dyn Task<Input, Output>> = Arc::new(task(move |input: &Input, hooks| {
        let calls = calls.clone();
        let gate = gate.clone();
        Box::pin(async move {
            gate.wait().await;
            calls.fetch_add(1, Ordering::SeqCst);
            hooks.add_tag("called");
            Ok(Output(input.0 * 2))
        })
    }));
    let one = Input(1);
    let two = Input(2);
    let first = TaskHooks::local(None);
    let second = TaskHooks::local(None);
    let (one, two) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(task.run(&one, &first), task.run(&two, &second))
    })
    .await
    .unwrap();
    assert_eq!(one.unwrap().0, 2);
    assert_eq!(two.unwrap().0, 4);
    assert_eq!(observed.load(Ordering::SeqCst), 2);
    assert_eq!(first.get_tags().unwrap(), ["called"]);
    assert_eq!(second.get_tags().unwrap(), ["called"]);
}

#[tokio::test]
async fn scorer_helpers_accept_borrowed_and_owned_names_and_captured_state() {
    let local_name = String::from("borrowed-name");
    let borrowed = scorer(
        local_name.as_str(),
        |args: ScorerArgs<'_, Input, Output>| {
            Box::pin(async move {
                tokio::task::yield_now().await;
                Ok(vec![Score::new("value", args.input.0 as f64)])
            })
        },
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let owned = scorer(
        String::from("owned-name"),
        move |args: ScorerArgs<'_, Input, Output>| {
            let calls = calls.clone();
            Box::pin(async move {
                tokio::task::yield_now().await;
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(vec![Score::new("value", args.output.0 as f64)])
            })
        },
    );
    let input = Input(2);
    let output = Output(4);
    assert_eq!(borrowed.name(), "borrowed-name");
    assert_eq!(owned.name(), "owned-name");
    for callback in [&borrowed as &dyn Scorer<Input, Output>, &owned] {
        assert_eq!(
            callback
                .score(ScorerArgs {
                    input: &input,
                    output: &output,
                    expected: None,
                    metadata: None
                })
                .await
                .unwrap()
                .len(),
            1
        );
    }
    assert_eq!(observed.load(Ordering::SeqCst), 1);
}

struct UntouchedSource(Arc<AtomicBool>);
#[async_trait]
impl Dataset<usize, usize> for UntouchedSource {
    async fn next(&mut self) -> braintrust_sdk_rust::Result<Option<Case<usize, usize>>> {
        self.0.store(true, Ordering::SeqCst);
        panic!("invalid plans must not read their source");
    }
}

#[tokio::test]
async fn invalid_concurrency_and_names_fail_before_login_recording_or_source_access() {
    let server = wiremock::MockServer::start().await;
    let client = BraintrustClient::builder()
        .skip_login(true)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap();
    let evaluator = Evaluator::new(client);
    let accessed = Arc::new(AtomicBool::new(false));
    for (concurrency, names, expected) in [
        (0, vec!["one"], EvalConfigError::ZeroConcurrency),
        (
            1,
            vec![" \t"],
            EvalConfigError::EmptyScorerName { index: 0 },
        ),
        (
            1,
            vec!["same", "same"],
            EvalConfigError::DuplicateScorerName {
                name: "same".into(),
            },
        ),
    ] {
        let scorers: Vec<Box<dyn Scorer<usize, usize>>> = names
            .into_iter()
            .map(|name| {
                Box::new(scorer(name, |_args| Box::pin(async { Ok(vec![]) })))
                    as Box<dyn Scorer<usize, usize>>
            })
            .collect();
        let opts = EvalOpts::builder()
            .name("invalid".into())
            .dataset(Box::new(UntouchedSource(accessed.clone())) as Box<dyn Dataset<usize, usize>>)
            .task(Box::new(SimpleFnTask::new(|input: &usize| Ok(*input))))
            .scorers(scorers)
            .max_concurrency(concurrency)
            .quiet(true)
            .build();
        assert_eq!(opts.validate(), Err(expected.clone()));
        assert!(
            matches!(evaluator.run(opts).await, Err(BraintrustError::InvalidConfig(message)) if message == expected.to_string())
        );
    }
    assert!(!accessed.load(Ordering::SeqCst));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn duplicate_names_are_rejected_across_custom_tuple_and_builtin_scorers() {
    struct Custom;
    #[async_trait]
    impl Scorer<usize, usize> for Custom {
        fn name(&self) -> &str {
            "exact_match"
        }
        async fn score(&self, _: ScorerArgs<'_, usize, usize>) -> anyhow::Result<Scores> {
            Ok(vec![])
        }
    }
    for callback in [
        Box::new(Custom) as Box<dyn Scorer<usize, usize>>,
        Box::new(scorer(String::from("exact_match"), |_args| {
            Box::pin(async { Ok(vec![]) })
        })),
    ] {
        let opts = EvalOpts::builder()
            .name("duplicate".into())
            .dataset(Vec::<Case<usize, usize>>::new().into())
            .task(Box::new(SimpleFnTask::new(|input: &usize| Ok(*input))))
            .scorers(vec![Box::new(ExactMatch::new()), callback])
            .build();
        assert_eq!(
            opts.validate(),
            Err(EvalConfigError::DuplicateScorerName {
                name: "exact_match".into()
            })
        );
    }
}
