//! Example demonstrating how to use the MemoryLogger for testing
//!
//! This example shows how to:
//! 1. Create a memory logger for testing
//! 2. Log events without making API calls
//! 3. Inspect and validate logged events
//! 4. Use test helpers for assertions

use braintrust_sdk::memory_logger::MemoryLogger;
use braintrust_sdk::queue::EventQueue;
use braintrust_sdk::span::{create_test_span, Span};
use braintrust_sdk::test_helpers::*;
use braintrust_sdk::types::{SpanType, StartSpanOptions};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🧪 Memory Logger Testing Example\n");

    // Example 1: Basic memory logger usage
    println!("Example 1: Basic Memory Logger");
    println!("─────────────────────────────────");
    basic_memory_logger_example()?;
    println!();

    // Example 2: Testing with EventQueue
    println!("Example 2: Testing Spans with EventQueue");
    println!("─────────────────────────────────────────");
    event_queue_example().await?;
    println!();

    // Example 3: Nested spans testing
    println!("Example 3: Testing Nested Spans");
    println!("────────────────────────────────");
    nested_spans_example().await?;
    println!();

    // Example 4: Using test helpers
    println!("Example 4: Using Test Helpers");
    println!("──────────────────────────────");
    test_helpers_example().await?;
    println!();

    println!("✅ All examples completed successfully!");

    Ok(())
}

/// Example 1: Basic memory logger usage
fn basic_memory_logger_example() -> Result<(), Box<dyn std::error::Error>> {
    let memory_logger = MemoryLogger::new();

    // Create and log some test events
    let mut event1 = create_test_event("test_span_1");
    event1.input = Some(json!({"query": "What is 2+2?"}));
    event1.output = Some(json!({"answer": "4"}));

    let mut event2 = create_test_event("test_span_2");
    event2.input = Some(json!({"query": "What is 3+3?"}));
    event2.output = Some(json!({"answer": "6"}));

    memory_logger.log(event1);
    memory_logger.log(event2);

    println!("✓ Logged {} events", memory_logger.len());

    // Drain events and inspect
    let events = memory_logger.drain();
    println!("✓ Drained {} events", events.len());

    // Validate events
    for (i, event) in events.iter().enumerate() {
        println!("  Event {}: {}", i + 1,
            event.span_attributes.as_ref()
                .and_then(|a| a.name.as_ref())
                .unwrap_or(&"unnamed".to_string())
        );
    }

    Ok(())
}

/// Example 2: Testing with EventQueue
async fn event_queue_example() -> Result<(), Box<dyn std::error::Error>> {
    let queue = Arc::new(EventQueue::new(100));

    // Create a span
    let span = create_test_span(
        queue.clone(),
        StartSpanOptions::new()
            .with_name("llm_call")
            .with_type(SpanType::Llm),
        Some(TEST_PROJECT_ID.to_string()),
    );

    // Log LLM interaction
    span.log(json!({
        "input": {
            "messages": [
                {"role": "user", "content": "Explain quantum computing"}
            ]
        },
        "output": {
            "content": "Quantum computing uses quantum mechanics..."
        },
        "metadata": {
            "model": "gpt-4",
            "temperature": 0.7
        },
        "metrics": {
            "prompt_tokens": 10,
            "completion_tokens": 50,
            "tokens": 60
        }
    }))?;

    span.end()?;

    // Wait a moment for events to be queued
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Drain and inspect
    let events = queue.drain_merged();
    println!("✓ Captured {} events", events.len());

    let event = &events[0];
    println!("✓ Span name: {}",
        event.span_attributes.as_ref()
            .and_then(|a| a.name.as_ref())
            .unwrap_or(&"unnamed".to_string())
    );
    println!("✓ Has input: {}", event.input.is_some());
    println!("✓ Has output: {}", event.output.is_some());
    println!("✓ Has metrics: {}", event.metrics.is_some());

    Ok(())
}

/// Example 3: Testing nested spans
async fn nested_spans_example() -> Result<(), Box<dyn std::error::Error>> {
    let queue = Arc::new(EventQueue::new(100));

    // Create parent span
    let parent = create_test_span(
        queue.clone(),
        StartSpanOptions::new()
            .with_name("parent_task")
            .with_type(SpanType::Task),
        Some(TEST_PROJECT_ID.to_string()),
    );

    parent.log(json!({
        "input": "Process documents"
    }))?;

    // Create child spans
    for i in 1..=3 {
        let child = create_test_span(
            queue.clone(),
            StartSpanOptions::new()
                .with_name(format!("child_task_{}", i))
                .with_type(SpanType::Function)
                .with_parent(&parent),
            Some(TEST_PROJECT_ID.to_string()),
        );

        child.log(json!({
            "input": format!("Document {}", i),
            "output": format!("Processed document {}", i)
        }))?;

        child.end()?;
    }

    parent.end()?;

    // Wait and drain
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    let events = queue.drain_merged();

    println!("✓ Total events: {}", events.len());

    // Find parent and children
    let parent_event = find_event_by_name(&events, "parent_task")
        .expect("Parent not found");

    println!("✓ Found parent span");

    let mut child_count = 0;
    for i in 1..=3 {
        let child_name = format!("child_task_{}", i);
        if let Some(child_event) = find_event_by_name(&events, &child_name) {
            assert_parent_child_relationship(parent_event, child_event);
            child_count += 1;
        }
    }

    println!("✓ Verified {} parent-child relationships", child_count);

    Ok(())
}

/// Example 4: Using test helpers
async fn test_helpers_example() -> Result<(), Box<dyn std::error::Error>> {
    let queue = Arc::new(EventQueue::new(100));

    let span = create_test_span(
        queue.clone(),
        StartSpanOptions::new()
            .with_name("scored_task")
            .with_type(SpanType::Eval),
        Some(TEST_PROJECT_ID.to_string()),
    );

    span.log(json!({
        "input": "Translate: Hello",
        "output": "Bonjour",
        "scores": {
            "accuracy": 0.98,
            "fluency": 0.95
        }
    }))?;

    span.end()?;

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    let events = queue.drain_merged();

    let event = &events[0];

    // Use test helpers to validate
    println!("✓ Testing with helpers:");

    // Check event matches expected structure
    assert_event_matches(event, json!({
        "input": "Translate: Hello",
        "output": "Bonjour"
    }));
    println!("  ✓ Event structure matches");

    // Check scores
    let mut expected_scores = std::collections::HashMap::new();
    expected_scores.insert("accuracy".to_string(), 0.98);
    expected_scores.insert("fluency".to_string(), 0.95);
    assert_scores(event, expected_scores);
    println!("  ✓ Scores validated");

    // Check event has required fields
    assert_event_field(event, "id", |id: &String| !id.is_empty());
    println!("  ✓ Field validation passed");

    Ok(())
}
