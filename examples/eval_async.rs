use braintrust_sdk_rust::eval::*;

/// Demonstrates using BtEvalRunner, which is compatible with the `bt eval` CLI.
///
/// When run directly, it behaves like a standard eval runner. When invoked via
/// `bt eval`, it reads BT_EVAL_* environment variables to control behavior:
///   - BT_EVAL_LIST=1        — list eval names without running
///   - BT_EVAL_JSONL=1       — print summaries as JSONL
///   - BT_EVAL_SSE_SOCK=...  — send SSE events over a Unix socket
///   - BT_EVAL_SSE_ADDR=...  — send SSE events over TCP
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // BtEvalRunner reads BT_EVAL_* env vars automatically.
    // In non-list mode it creates a BraintrustClient from the environment.
    let mut runner = BtEvalRunner::from_env().await?;

    // Define test cases
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
        Case {
            input: "rust".to_string(),
            expected: Some("RUST".to_string()),
            ..Default::default()
        },
        Case {
            input: "braintrust".to_string(),
            expected: Some("BRAINTRUST".to_string()),
            ..Default::default()
        },
    ];

    // Share callbacks across runs; each run supplies its own source and options.
    let definition = EvalDefinition::new(
        Box::new(task(|input: &String, _hooks| {
            Box::pin(async move { Ok(input.to_uppercase()) })
        })),
        vec![
            Box::new(ExactMatch::new()),
            Box::new(LevenshteinScorer::new()),
            Box::new(scorer(
                "nonempty",
                |args: ScorerArgs<'_, String, String>| {
                    Box::pin(async move {
                        Ok(vec![Score::new(
                            "nonempty",
                            if args.output.is_empty() { 0.0 } else { 1.0 },
                        )])
                    })
                },
            )),
        ],
    )?;

    runner
        .eval(
            "my-project",
            EvalOpts::builder()
                .name("uppercase-eval".to_string())
                .source(cases.into())
                .definition(definition.clone())
                .max_concurrency(2)
                .build(),
        )
        .await?;

    let passed = runner.finish().await?;
    if !passed {
        std::process::exit(1);
    }

    Ok(())
}
