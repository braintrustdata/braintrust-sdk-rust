//! Integration tests for span lifecycle and logging

use braintrust_sdk::memory_logger::MemoryLogger;
use braintrust_sdk::test_helpers::*;
use braintrust_sdk::types::{SpanEvent, SpanType, StartSpanOptions, SpanAttributes, SpanMetrics};
use serde_json::json;

#[test]
fn test_span_event_creation() {
    let event = SpanEvent::new();

    // Verify basic properties
    assert!(!event.id.is_empty(), "Event should have an ID");
    assert!(event.created.is_some(), "Event should have a creation timestamp");
}

#[test]
fn test_span_event_with_data() {
    let mut event = SpanEvent::new();
    event.input = Some(json!({"question": "What is 2+2?"}));
    event.output = Some(json!({"answer": "4"}));
    event.metadata = Some(json!({"model": "test-model"}));

    assert_eq!(event.input, Some(json!({"question": "What is 2+2?"})));
    assert_eq!(event.output, Some(json!({"answer": "4"})));
    assert_eq!(event.metadata, Some(json!({"model": "test-model"})));
}

#[test]
fn test_span_attributes() {
    let mut event = SpanEvent::new();
    event.span_attributes = Some(SpanAttributes {
        name: Some("test_span".to_string()),
        span_type: Some(SpanType::Task),
    });

    let attrs = event.span_attributes.as_ref().unwrap();
    assert_eq!(attrs.name.as_ref().unwrap(), "test_span");
    assert_eq!(attrs.span_type, Some(SpanType::Task));
}

#[test]
fn test_span_types() {
    let types = vec![
        SpanType::Task,
        SpanType::Llm,
        SpanType::Function,
        SpanType::Eval,
        SpanType::Score,
        SpanType::Tool,
    ];

    for span_type in types {
        let mut event = SpanEvent::new();
        event.span_attributes = Some(SpanAttributes {
            name: Some("test".to_string()),
            span_type: Some(span_type),
        });

        assert_eq!(
            event.span_attributes.as_ref().unwrap().span_type,
            Some(span_type)
        );
    }
}

#[test]
fn test_span_metrics() {
    let mut event = SpanEvent::new();
    event.metrics = Some(SpanMetrics {
        start: Some(1000.0),
        end: Some(2000.0),
        prompt_tokens: Some(100),
        completion_tokens: Some(50),
        tokens: Some(150),
        estimated_cost: Some(0.005),
    });

    let metrics = event.metrics.as_ref().unwrap();
    assert_eq!(metrics.start, Some(1000.0));
    assert_eq!(metrics.end, Some(2000.0));
    assert_eq!(metrics.prompt_tokens, Some(100));
    assert_eq!(metrics.completion_tokens, Some(50));
    assert_eq!(metrics.tokens, Some(150));
    assert_eq!(metrics.estimated_cost, Some(0.005));
}

#[test]
fn test_span_parent_relationship() {
    let parent_id = uuid::Uuid::new_v4().to_string();

    let mut parent = SpanEvent::new();
    parent.span_id = Some(parent_id.clone());

    let mut child = SpanEvent::new();
    child.span_id = Some(uuid::Uuid::new_v4().to_string());
    child.span_parents = Some(vec![parent_id.clone()]);

    assert_eq!(child.span_parents.as_ref().unwrap()[0], parent_id);
}

#[test]
fn test_span_hierarchy() {
    let root_id = uuid::Uuid::new_v4().to_string();

    let mut parent = SpanEvent::new();
    parent.span_id = Some(uuid::Uuid::new_v4().to_string());
    parent.root_span_id = Some(root_id.clone());

    let mut child = SpanEvent::new();
    child.span_id = Some(uuid::Uuid::new_v4().to_string());
    child.root_span_id = Some(root_id.clone());
    child.span_parents = Some(vec![parent.span_id.clone().unwrap()]);

    // Verify they're in the same trace
    assert_same_trace(&parent, &child);
}

#[test]
fn test_memory_logger_basic() {
    let logger = MemoryLogger::new();

    let mut event1 = SpanEvent::new();
    event1.input = Some(json!({"test": "data1"}));

    let mut event2 = SpanEvent::new();
    event2.input = Some(json!({"test": "data2"}));

    logger.log(event1);
    logger.log(event2);

    assert_eq!(logger.len(), 2);

    let events = logger.drain();
    assert_eq!(events.len(), 2);
    assert_eq!(logger.len(), 0);
}

#[test]
fn test_memory_logger_merge_same_id() {
    let logger = MemoryLogger::new();
    let event_id = uuid::Uuid::new_v4().to_string();

    // First event (not a merge)
    let mut event1 = SpanEvent::new();
    event1.id = event_id.clone();
    event1.input = Some(json!({"x": 1}));
    event1.is_merge = Some(false);

    // Second event (is a merge)
    let mut event2 = SpanEvent::new();
    event2.id = event_id.clone();
    event2.output = Some(json!({"y": 2}));
    event2.is_merge = Some(true);

    logger.log(event1);
    logger.log(event2);

    let events = logger.drain();

    // Should be merged into one event
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].input, Some(json!({"x": 1})));
    assert_eq!(events[0].output, Some(json!({"y": 2})));
}

#[test]
fn test_event_merge_metadata() {
    let logger = MemoryLogger::new();
    let event_id = uuid::Uuid::new_v4().to_string();

    let mut event1 = SpanEvent::new();
    event1.id = event_id.clone();
    event1.metadata = Some(json!({"model": "gpt-4", "temperature": 0.7}));
    event1.is_merge = Some(false);

    let mut event2 = SpanEvent::new();
    event2.id = event_id.clone();
    event2.metadata = Some(json!({"max_tokens": 100}));
    event2.is_merge = Some(true);

    logger.log(event1);
    logger.log(event2);

    let events = logger.drain();
    assert_eq!(events.len(), 1);

    // Metadata should be deep merged
    let metadata = events[0].metadata.as_ref().unwrap();
    assert_eq!(metadata["model"], "gpt-4");
    assert_eq!(metadata["temperature"], 0.7);
    assert_eq!(metadata["max_tokens"], 100);
}

#[test]
fn test_event_merge_scores() {
    let logger = MemoryLogger::new();
    let event_id = uuid::Uuid::new_v4().to_string();

    let mut event1 = SpanEvent::new();
    event1.id = event_id.clone();
    let mut scores1 = std::collections::HashMap::new();
    scores1.insert("accuracy".to_string(), 0.95);
    event1.scores = Some(scores1);
    event1.is_merge = Some(false);

    let mut event2 = SpanEvent::new();
    event2.id = event_id.clone();
    let mut scores2 = std::collections::HashMap::new();
    scores2.insert("latency".to_string(), 0.12);
    event2.scores = Some(scores2);
    event2.is_merge = Some(true);

    logger.log(event1);
    logger.log(event2);

    let events = logger.drain();
    assert_eq!(events.len(), 1);

    // Scores should be merged
    let scores = events[0].scores.as_ref().unwrap();
    assert_eq!(scores.get("accuracy").unwrap(), &0.95);
    assert_eq!(scores.get("latency").unwrap(), &0.12);
}

#[test]
fn test_start_span_options() {
    let options = StartSpanOptions::new()
        .with_name("test_span")
        .with_type(SpanType::Llm);

    assert_eq!(options.name.as_ref().unwrap(), "test_span");
    assert_eq!(options.span_type, Some(SpanType::Llm));
    // Parent testing is done in integration tests with actual spans
}

#[test]
fn test_helper_find_event_by_name() {
    let event1 = create_test_event("span1");
    let event2 = create_test_event("span2");
    let event3 = create_test_event("span3");

    let events = vec![event1, event2, event3];

    let found = find_event_by_name(&events, "span2");
    assert!(found.is_some());
    assert_eq!(
        found.unwrap().span_attributes.as_ref().unwrap().name.as_ref().unwrap(),
        "span2"
    );

    let not_found = find_event_by_name(&events, "nonexistent");
    assert!(not_found.is_none());
}

#[test]
fn test_helper_assert_parent_child_relationship() {
    let parent = create_test_event("parent");
    let parent_span_id = parent.span_id.clone().unwrap();

    let mut child = create_test_event("child");
    child.span_parents = Some(vec![parent_span_id.clone()]);

    // Should not panic
    assert_parent_child_relationship(&parent, &child);
}

#[test]
#[should_panic(expected = "Child event does not list parent")]
fn test_helper_assert_parent_child_relationship_fails() {
    let parent = create_test_event("parent");
    let mut child = create_test_event("child");
    child.span_parents = Some(vec!["wrong-parent-id".to_string()]);

    assert_parent_child_relationship(&parent, &child);
}

#[test]
fn test_helper_assert_scores() {
    let mut event = create_test_event("test");
    let mut scores = std::collections::HashMap::new();
    scores.insert("accuracy".to_string(), 0.95);
    scores.insert("f1_score".to_string(), 0.88);
    event.scores = Some(scores.clone());

    // Should not panic
    assert_scores(&event, scores);
}

#[test]
fn test_event_serialization() {
    let mut event = SpanEvent::new();
    event.input = Some(json!({"test": "data"}));
    event.span_attributes = Some(SpanAttributes {
        name: Some("test_span".to_string()),
        span_type: Some(SpanType::Task),
    });

    // Should be able to serialize to JSON
    let json = serde_json::to_string(&event).expect("Failed to serialize");
    assert!(json.contains("test_span"));
    assert!(json.contains("task"));

    // Should be able to deserialize back
    let deserialized: SpanEvent = serde_json::from_str(&json).expect("Failed to deserialize");
    assert_eq!(deserialized.input, event.input);
    assert_eq!(
        deserialized.span_attributes.as_ref().unwrap().name,
        event.span_attributes.as_ref().unwrap().name
    );
}

#[test]
fn test_deep_json_merge() {
    let logger = MemoryLogger::new();
    let event_id = uuid::Uuid::new_v4().to_string();

    let mut event1 = SpanEvent::new();
    event1.id = event_id.clone();
    event1.input = Some(json!({
        "user": {
            "name": "Alice",
            "age": 30
        },
        "query": "test"
    }));
    event1.is_merge = Some(false);

    let mut event2 = SpanEvent::new();
    event2.id = event_id.clone();
    event2.input = Some(json!({
        "user": {
            "email": "alice@example.com"
        }
    }));
    event2.is_merge = Some(true);

    logger.log(event1);
    logger.log(event2);

    let events = logger.drain();
    assert_eq!(events.len(), 1);

    let input = events[0].input.as_ref().unwrap();
    assert_eq!(input["query"], "test");
    assert_eq!(input["user"]["name"], "Alice");
    assert_eq!(input["user"]["age"], 30);
    assert_eq!(input["user"]["email"], "alice@example.com");
}
