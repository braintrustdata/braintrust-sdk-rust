//! Simple trace example demonstrating basic Braintrust SDK usage

use braintrust_sdk::{Logger, LoggerOptions, StartSpanOptions, SpanType};
use serde_json::json;
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    println!("🚀 Starting Braintrust Rust SDK Example");

    // Initialize logger with project name
    let logger = Logger::new_async(LoggerOptions::new("rust sdk project")).await?;
    println!("✅ Logger initialized for project: {}", logger.project_id());

    // Create a root span for the conversation
    let root_span = logger.start_span(Some(
        StartSpanOptions::new()
            .with_name("simple_conversation")
            .with_type(SpanType::Task),
    ));

    println!("📝 Created root span: {}", root_span.id());

    // Log input and output
    root_span.log(json!({
        "input": "What is 2 + 2?",
        "output": "The answer is 4.",
        "metadata": {
            "model": "rust-example",
            "version": "0.1.0"
        }
    }))?;

    println!("✅ Logged conversation data");

    // End the span
    root_span.end()?;
    println!("✅ Ended span");

    // Create another span to demonstrate multiple traces
    let span2 = logger.start_span(Some(
        StartSpanOptions::new()
            .with_name("calculation_task")
            .with_type(SpanType::Task),
    ));

    span2.log(json!({
        "input": "Calculate 10 * 5",
        "output": "50",
        "metadata": {
            "operation": "multiplication"
        }
    }))?;

    span2.end()?;
    println!("✅ Created and logged second span");

    // Flush to ensure all events are sent
    logger.flush().await?;
    println!("✅ Flushed all events to Braintrust");

    println!("🎉 Example completed successfully!");
    println!("   View your traces at: https://www.braintrust.dev");

    Ok(())
}
