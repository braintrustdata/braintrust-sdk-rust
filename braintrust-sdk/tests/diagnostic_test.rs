//! Diagnostic test to understand event merging behavior

use braintrust_sdk::queue::EventQueue;
use braintrust_sdk::span::{create_test_span, Span};
use braintrust_sdk::types::{SpanType, StartSpanOptions};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn test_event_merging_diagnostic() {
    let queue = Arc::new(EventQueue::new(100));
    let options = StartSpanOptions::new()
        .with_name("test_span")
        .with_type(SpanType::Task);

    let span = create_test_span(queue.clone(), options, Some("test-project".to_string()));

    println!("\nCreated span with ID: {}", span.id());
    println!("Span span_id: {}", span.span_id());

    // Log data
    span.log(json!({
        "input": "test input",
        "output": "test output"
    })).unwrap();

    println!("Logged data to span");

    // Export to see current state
    let exported = span.export();
    println!("Exported span input: {:?}", exported.input);
    println!("Exported span output: {:?}", exported.output);

    // End span
    span.end().unwrap();

    println!("Ended span");

    // Wait for events to be queued
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Drain and merge (like the background processor does)
    let events = queue.drain_merged();
    println!("\nDrained and merged {} events from queue", events.len());

    for (i, event) in events.iter().enumerate() {
        println!("\n=== Event {} ===", i);
        println!("  ID: {}", event.id);
        println!("  span_id: {:?}", event.span_id);
        println!("  is_merge: {:?}", event.is_merge);
        println!("  input: {:?}", event.input);
        println!("  output: {:?}", event.output);
        println!("  metrics.start: {:?}", event.metrics.as_ref().and_then(|m| m.start));
        println!("  metrics.end: {:?}", event.metrics.as_ref().and_then(|m| m.end));
        println!("  span_attributes.name: {:?}",
            event.span_attributes.as_ref().and_then(|a| a.name.as_ref()));
    }

    // The merged event should have all the data
    assert_eq!(events.len(), 1, "Should have exactly 1 merged event");
    let merged = &events[0];

    println!("\n=== Checking merged event ===");
    println!("Has input: {}", merged.input.is_some());
    println!("Has output: {}", merged.output.is_some());
    println!("Has end time: {}", merged.metrics.as_ref().and_then(|m| m.end).is_some());
}
