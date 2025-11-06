//! Integration tests for nested spans and complex hierarchies

use braintrust_sdk::queue::EventQueue;
use braintrust_sdk::span::{create_test_span, Span};
use braintrust_sdk::test_helpers::*;
use braintrust_sdk::types::{SpanType, StartSpanOptions};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn test_single_span_with_queue() {
    let queue = Arc::new(EventQueue::new(100));
    let options = StartSpanOptions::new()
        .with_name("test_span")
        .with_type(SpanType::Task);

    let span = create_test_span(queue.clone(), options, Some(TEST_PROJECT_ID.to_string()));

    span.log(json!({
        "input": "test input",
        "output": "test output"
    }))
    .unwrap();

    span.end().unwrap();

    // Give a moment for events to be queued
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    let events = queue.drain_merged();

    // Should have events: initial span creation + log update + end update
    assert!(events.len() >= 1, "Expected at least 1 event");

    // After merging, should have combined data
    let merged = &events[0];
    assert_eq!(merged.input, Some(json!("test input")));
    assert_eq!(merged.output, Some(json!("test output")));
}

#[tokio::test]
async fn test_nested_spans_parent_child() {
    let queue = Arc::new(EventQueue::new(100));

    // Create parent span
    let parent_options = StartSpanOptions::new()
        .with_name("parent_span")
        .with_type(SpanType::Task);

    let parent = create_test_span(queue.clone(), parent_options, Some(TEST_PROJECT_ID.to_string()));

    parent
        .log(json!({
            "input": "parent input"
        }))
        .unwrap();

    // Create child span with parent reference
    let child_options = StartSpanOptions::new()
        .with_name("child_span")
        .with_type(SpanType::Function)
        .with_parent(&parent);

    let child = create_test_span(queue.clone(), child_options, Some(TEST_PROJECT_ID.to_string()));

    child
        .log(json!({
            "input": "child input",
            "output": "child output"
        }))
        .unwrap();

    child.end().unwrap();
    parent.end().unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    let events = queue.drain_merged();

    // Find parent and child by span_id
    let parent_span_id = parent.span_id();
    let parent_event = find_event_by_span_id(&events, &parent_span_id)
        .expect("Parent event not found");

    let child_span_id = child.span_id();
    let child_event = find_event_by_span_id(&events, &child_span_id)
        .expect("Child event not found");

    // Verify parent-child relationship
    assert_parent_child_relationship(parent_event, child_event);

    // Verify they're in the same trace
    assert_same_trace(parent_event, child_event);
}

#[tokio::test]
async fn test_three_level_hierarchy() {
    let queue = Arc::new(EventQueue::new(100));

    // Level 1: Root
    let root = create_test_span(
        queue.clone(),
        StartSpanOptions::new().with_name("root"),
        Some(TEST_PROJECT_ID.to_string()),
    );

    // Level 2: Middle
    let middle = create_test_span(
        queue.clone(),
        StartSpanOptions::new()
            .with_name("middle")
            .with_parent(&root),
        Some(TEST_PROJECT_ID.to_string()),
    );

    // Level 3: Leaf
    let leaf = create_test_span(
        queue.clone(),
        StartSpanOptions::new()
            .with_name("leaf")
            .with_parent(&middle),
        Some(TEST_PROJECT_ID.to_string()),
    );

    root.log(json!({"level": "root"})).unwrap();
    middle.log(json!({"level": "middle"})).unwrap();
    leaf.log(json!({"level": "leaf"})).unwrap();

    leaf.end().unwrap();
    middle.end().unwrap();
    root.end().unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    let events = queue.drain_merged();

    let root_event = find_event_by_name(&events, "root").expect("Root not found");
    let middle_event = find_event_by_name(&events, "middle").expect("Middle not found");
    let leaf_event = find_event_by_name(&events, "leaf").expect("Leaf not found");

    // Verify root -> middle relationship
    assert_parent_child_relationship(root_event, middle_event);

    // Verify middle -> leaf relationship
    assert_parent_child_relationship(middle_event, leaf_event);

    // All should be in the same trace
    assert_same_trace(root_event, middle_event);
    assert_same_trace(middle_event, leaf_event);
}

#[tokio::test]
async fn test_multiple_children() {
    let queue = Arc::new(EventQueue::new(100));

    // Parent
    let parent = create_test_span(
        queue.clone(),
        StartSpanOptions::new().with_name("parent"),
        Some(TEST_PROJECT_ID.to_string()),
    );

    // Create three children
    let child1 = create_test_span(
        queue.clone(),
        StartSpanOptions::new()
            .with_name("child1")
            .with_parent(&parent),
        Some(TEST_PROJECT_ID.to_string()),
    );

    let child2 = create_test_span(
        queue.clone(),
        StartSpanOptions::new()
            .with_name("child2")
            .with_parent(&parent),
        Some(TEST_PROJECT_ID.to_string()),
    );

    let child3 = create_test_span(
        queue.clone(),
        StartSpanOptions::new()
            .with_name("child3")
            .with_parent(&parent),
        Some(TEST_PROJECT_ID.to_string()),
    );

    parent.log(json!({"data": "parent"})).unwrap();
    child1.log(json!({"data": "child1"})).unwrap();
    child2.log(json!({"data": "child2"})).unwrap();
    child3.log(json!({"data": "child3"})).unwrap();

    child1.end().unwrap();
    child2.end().unwrap();
    child3.end().unwrap();
    parent.end().unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    let events = queue.drain_merged();

    let parent_event = find_event_by_name(&events, "parent").expect("Parent not found");

    // Verify all three children have parent as their parent
    for child_name in &["child1", "child2", "child3"] {
        let child_event = find_event_by_name(&events, child_name)
            .unwrap_or_else(|| panic!("{} not found", child_name));
        assert_parent_child_relationship(parent_event, child_event);
    }
}

#[tokio::test]
async fn test_span_with_llm_type() {
    let queue = Arc::new(EventQueue::new(100));

    let span = create_test_span(
        queue.clone(),
        StartSpanOptions::new()
            .with_name("llm_call")
            .with_type(SpanType::Llm),
        Some(TEST_PROJECT_ID.to_string()),
    );

    span.log(json!({
        "input": {"messages": [{"role": "user", "content": "Hello"}]},
        "output": {"content": "Hi there!"},
        "metadata": {
            "model": "gpt-4",
            "temperature": 0.7
        },
        "metrics": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "tokens": 15
        }
    }))
    .unwrap();

    span.end().unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    let events = queue.drain_merged();
    let event = &events[0];

    // Verify LLM-specific data
    assert!(event.metadata.is_some());
    assert!(event.metrics.is_some());

    let metadata = event.metadata.as_ref().unwrap();
    assert_eq!(metadata["model"], "gpt-4");
    assert_eq!(metadata["temperature"], 0.7);
}

#[tokio::test]
async fn test_span_with_scores() {
    let queue = Arc::new(EventQueue::new(100));

    let span = create_test_span(
        queue.clone(),
        StartSpanOptions::new()
            .with_name("evaluated_span")
            .with_type(SpanType::Task),
        Some(TEST_PROJECT_ID.to_string()),
    );

    span.log(json!({
        "input": "test",
        "output": "result",
        "scores": {
            "accuracy": 0.95,
            "latency": 0.12,
            "quality": 0.88
        }
    }))
    .unwrap();

    span.end().unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    let events = queue.drain_merged();
    let event = &events[0];

    let mut expected_scores = std::collections::HashMap::new();
    expected_scores.insert("accuracy".to_string(), 0.95);
    expected_scores.insert("latency".to_string(), 0.12);
    expected_scores.insert("quality".to_string(), 0.88);

    assert_scores(event, expected_scores);
}

#[tokio::test]
async fn test_span_timing_metrics() {
    let queue = Arc::new(EventQueue::new(100));

    let span = create_test_span(
        queue.clone(),
        StartSpanOptions::new().with_name("timed_span"),
        Some(TEST_PROJECT_ID.to_string()),
    );

    let start_time = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;

    // Simulate some work
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    span.end().unwrap();

    let end_time = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    let events = queue.drain_merged();
    let event = &events[0];

    let metrics = event.metrics.as_ref().expect("Metrics not found");

    // Verify timing is reasonable
    assert!(metrics.start.is_some());
    assert!(metrics.end.is_some());

    let actual_start = metrics.start.unwrap();
    let actual_end = metrics.end.unwrap();

    // Start should be around when we created the span
    assert!(
        actual_start >= start_time - 0.1 && actual_start <= start_time + 0.1,
        "Start time out of range"
    );

    // End should be after start
    assert!(actual_end > actual_start, "End time should be after start time");

    // Duration should be at least 100ms (0.1 seconds)
    let duration = actual_end - actual_start;
    assert!(duration >= 0.1, "Duration should be at least 0.1 seconds");
}

#[tokio::test]
async fn test_span_multiple_log_calls() {
    let queue = Arc::new(EventQueue::new(100));

    let span = create_test_span(
        queue.clone(),
        StartSpanOptions::new().with_name("multi_log"),
        Some(TEST_PROJECT_ID.to_string()),
    );

    // Log input
    span.log(json!({"input": "question"})).unwrap();

    // Simulate processing
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Log output
    span.log(json!({"output": "answer"})).unwrap();

    // Log metadata
    span.log(json!({"metadata": {"step": "complete"}}))
        .unwrap();

    span.end().unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    let events = queue.drain_merged();
    let merged = &events[0];

    // All logged data should be merged into one event
    assert_eq!(merged.input, Some(json!("question")));
    assert_eq!(merged.output, Some(json!("answer")));
    assert!(merged.metadata.is_some());
    assert_eq!(merged.metadata.as_ref().unwrap()["step"], "complete");
}

#[tokio::test]
async fn test_span_with_error() {
    let queue = Arc::new(EventQueue::new(100));

    let span = create_test_span(
        queue.clone(),
        StartSpanOptions::new().with_name("error_span"),
        Some(TEST_PROJECT_ID.to_string()),
    );

    span.log(json!({
        "input": "test",
        "error": "Something went wrong"
    }))
    .unwrap();

    span.end().unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    let events = queue.drain_merged();
    let event = &events[0];

    assert_eq!(event.error, Some("Something went wrong".to_string()));
}

#[tokio::test]
async fn test_span_export() {
    let queue = Arc::new(EventQueue::new(100));

    let span = create_test_span(
        queue.clone(),
        StartSpanOptions::new().with_name("export_test"),
        Some(TEST_PROJECT_ID.to_string()),
    );

    span.log(json!({
        "input": "test",
        "output": "result"
    }))
    .unwrap();

    // Export should return current state
    let exported = span.export();

    assert_eq!(exported.input, Some(json!("test")));
    assert_eq!(exported.output, Some(json!("result")));
    assert_eq!(
        exported
            .span_attributes
            .as_ref()
            .unwrap()
            .name
            .as_ref()
            .unwrap(),
        "export_test"
    );
}

#[tokio::test]
async fn test_concurrent_spans() {
    let queue = Arc::new(EventQueue::new(100));

    // Create multiple spans concurrently
    let mut handles = vec![];

    for i in 0..5 {
        let queue_clone = queue.clone();
        let handle = tokio::spawn(async move {
            let span = create_test_span(
                queue_clone,
                StartSpanOptions::new().with_name(format!("span_{}", i)),
                Some(TEST_PROJECT_ID.to_string()),
            );

            span.log(json!({"data": i})).unwrap();
            span.end().unwrap();
        });
        handles.push(handle);
    }

    // Wait for all to complete
    for handle in handles {
        handle.await.unwrap();
    }

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let events = queue.drain_merged();

    // Should have 5 merged events (one per span)
    assert!(events.len() >= 5, "Expected at least 5 events");

    // All span names should be unique
    let names: Vec<_> = events
        .iter()
        .filter_map(|e| {
            e.span_attributes
                .as_ref()
                .and_then(|a| a.name.as_ref())
                .cloned()
        })
        .collect();

    assert!(names.len() >= 5);
}
