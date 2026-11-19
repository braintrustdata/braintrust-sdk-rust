use braintrust_sdk_rust::eval::*;
use braintrust_sdk_rust::*;

fn main() -> anyhow::Result<()> {
    // The client's owned runtime drives work; this thread only waits for tokens.
    let client = BraintrustClient::builder().build().blocking_wait()?;

    // Define test cases
    let cases = vec![
        Case::builder()
            .input("hello".to_string())
            .expected("HELLO".to_string())
            .build(),
        Case::builder()
            .input("world".to_string())
            .expected("WORLD".to_string())
            .build(),
        Case::builder()
            .input("sync".to_string())
            .expected("SYNC".to_string())
            .build(),
    ];

    // Define a simple sync task: uppercase the input
    let task = SyncFnTask::new(|input: &String, _hooks| Ok(input.to_uppercase()));

    // Define scorers (they work in both sync and async contexts)
    let scorers: Vec<Box<dyn Scorer<String, String>>> = vec![Box::new(ExactMatch::new())];

    // Run evaluation synchronously
    println!("Starting synchronous evaluation...\n");

    let definition = EvalDefinition::new(Box::new(task), scorers)?;
    let completion = client.eval(
        EvalOpts::builder()
            .name("sync-uppercase-eval".to_string())
            .source(cases.into())
            .definition(definition.clone())
            .build(),
    );
    assert!(completion.is_ok());
    let result = completion.blocking_wait()?;

    // Print results
    println!("\n=== Results ===\n");
    println!("Total cases: {}", result.total_cases());
    println!("Successful: {}", result.successful_cases());
    println!("Failed: {}", result.failed_cases());

    println!("\n=== Score Statistics ===\n");
    for stat in result.score_stats() {
        println!(
            "{}: mean={:.3}, min={:.3}, max={:.3}",
            stat.name(),
            stat.mean(),
            stat.min(),
            stat.max()
        );
    }

    Ok(())
}
