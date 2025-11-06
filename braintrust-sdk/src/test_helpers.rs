//! Test helpers and utilities for testing the Braintrust SDK
//!
//! This module provides helper functions, assertion utilities, and test fixtures
//! to make testing easier and more ergonomic.
//!
//! # Example
//!
//! ```no_run
//! use braintrust_sdk::test_helpers::*;
//! use braintrust_sdk::SpanEvent;
//! use serde_json::json;
//!
//! #[test]
//! fn test_span_logging() {
//!     let event = SpanEvent::new();
//!
//!     assert_event_field(&event, "id", |id: &String| !id.is_empty());
//! }
//! ```

use crate::types::{SpanEvent, SpanAttributes, SpanMetrics};
use serde_json::Value;
use std::collections::HashMap;

/// Test constants
pub const TEST_API_KEY: &str = "test-api-key-12345";
pub const TEST_ORG_ID: &str = "test-org-id";
pub const TEST_ORG_NAME: &str = "Test Organization";
pub const TEST_PROJECT_ID: &str = "test-project-id";
pub const TEST_PROJECT_NAME: &str = "Test Project";

/// Assert that an event matches the expected partial structure
///
/// This function allows for flexible matching where:
/// - Only specified fields in `expected` are checked
/// - Nested objects are supported
/// - Validator functions can be used for dynamic checks
///
/// # Example
///
/// ```no_run
/// use braintrust_sdk::test_helpers::*;
/// use braintrust_sdk::SpanEvent;
/// use serde_json::json;
///
/// let mut event = SpanEvent::new();
/// event.input = Some(json!({"x": 1}));
/// event.output = Some(json!({"y": 2}));
///
/// assert_event_matches(&event, json!({
///     "input": {"x": 1},
///     "output": {"y": 2}
/// }));
/// ```
pub fn assert_event_matches(event: &SpanEvent, expected: Value) {
    let actual = serde_json::to_value(event).expect("Failed to serialize event");
    assert_json_matches(&actual, &expected, "root");
}

/// Assert that a JSON value matches the expected partial structure
///
/// This is a recursive function that handles nested objects and arrays.
fn assert_json_matches(actual: &Value, expected: &Value, path: &str) {
    match (actual, expected) {
        (Value::Object(actual_map), Value::Object(expected_map)) => {
            for (key, expected_value) in expected_map {
                let new_path = format!("{}.{}", path, key);

                let actual_value = actual_map.get(key)
                    .unwrap_or_else(|| panic!("Expected key '{}' not found at path '{}'", key, new_path));

                assert_json_matches(actual_value, expected_value, &new_path);
            }
        }
        (Value::Array(actual_arr), Value::Array(expected_arr)) => {
            assert_eq!(
                actual_arr.len(),
                expected_arr.len(),
                "Array length mismatch at path '{}': expected {}, got {}",
                path,
                expected_arr.len(),
                actual_arr.len()
            );

            for (i, (actual_item, expected_item)) in actual_arr.iter().zip(expected_arr.iter()).enumerate() {
                let new_path = format!("{}[{}]", path, i);
                assert_json_matches(actual_item, expected_item, &new_path);
            }
        }
        (Value::Null, Value::Null) => {}
        (actual, expected) => {
            assert_eq!(
                actual, expected,
                "Value mismatch at path '{}': expected {:?}, got {:?}",
                path, expected, actual
            );
        }
    }
}

/// Assert that a specific field in an event satisfies a predicate
///
/// # Example
///
/// ```no_run
/// use braintrust_sdk::test_helpers::*;
/// use braintrust_sdk::SpanEvent;
///
/// let event = SpanEvent::new();
///
/// // Check that ID is not empty
/// assert_event_field(&event, "id", |id: &String| !id.is_empty());
/// ```
pub fn assert_event_field<T, F>(event: &SpanEvent, field: &str, validator: F)
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(&T) -> bool,
{
    let value = serde_json::to_value(event).expect("Failed to serialize event");
    let field_value = value
        .get(field)
        .unwrap_or_else(|| panic!("Field '{}' not found in event", field));

    let typed_value: T = serde_json::from_value(field_value.clone())
        .unwrap_or_else(|e| panic!("Failed to deserialize field '{}': {}", field, e));

    assert!(
        validator(&typed_value),
        "Validator failed for field '{}'",
        field
    );
}

/// Find a span event by its name in a list of events
///
/// # Example
///
/// ```no_run
/// use braintrust_sdk::test_helpers::*;
/// use braintrust_sdk::SpanEvent;
///
/// let events = vec![/* ... */];
/// let parent = find_event_by_name(&events, "parent_span")
///     .expect("Parent span not found");
/// ```
pub fn find_event_by_name<'a>(events: &'a [SpanEvent], name: &str) -> Option<&'a SpanEvent> {
    events.iter().find(|event| {
        event
            .span_attributes
            .as_ref()
            .and_then(|attrs| attrs.name.as_ref())
            .map(|n| n == name)
            .unwrap_or(false)
    })
}

/// Find a span event by its span ID in a list of events
pub fn find_event_by_span_id<'a>(events: &'a [SpanEvent], span_id: &str) -> Option<&'a SpanEvent> {
    events.iter().find(|event| {
        event
            .span_id
            .as_ref()
            .map(|id| id == span_id)
            .unwrap_or(false)
    })
}

/// Assert that a child event has the correct parent relationship
///
/// # Example
///
/// ```no_run
/// use braintrust_sdk::test_helpers::*;
/// use braintrust_sdk::SpanEvent;
///
/// let events = vec![/* ... */];
/// let parent = find_event_by_name(&events, "parent").unwrap();
/// let child = find_event_by_name(&events, "child").unwrap();
///
/// assert_parent_child_relationship(parent, child);
/// ```
pub fn assert_parent_child_relationship(parent: &SpanEvent, child: &SpanEvent) {
    let parent_span_id = parent.span_id.as_ref()
        .expect("Parent event has no span_id");

    let child_parents = child.span_parents.as_ref()
        .expect("Child event has no span_parents");

    assert!(
        child_parents.contains(parent_span_id),
        "Child event does not list parent as a parent. Parent ID: {}, Child parents: {:?}",
        parent_span_id,
        child_parents
    );
}

/// Create a test span event with common test data
pub fn create_test_event(name: &str) -> SpanEvent {
    let mut event = SpanEvent::new();
    event.span_attributes = Some(SpanAttributes {
        name: Some(name.to_string()),
        span_type: None,
    });
    event.span_id = Some(uuid::Uuid::new_v4().to_string());
    event.root_span_id = Some(uuid::Uuid::new_v4().to_string());
    event.project_id = Some(TEST_PROJECT_ID.to_string());
    event
}

/// Assert that metrics are within expected ranges
///
/// This is useful for checking timing-related metrics that may vary
pub fn assert_metrics_in_range(
    metrics: &SpanMetrics,
    expected_start_min: f64,
    expected_start_max: f64,
    expected_end_min: Option<f64>,
    expected_end_max: Option<f64>,
) {
    if let Some(start) = metrics.start {
        assert!(
            start >= expected_start_min && start <= expected_start_max,
            "Start time {} is not in range [{}, {}]",
            start,
            expected_start_min,
            expected_start_max
        );
    } else {
        panic!("Metrics has no start time");
    }

    if let (Some(end_min), Some(end_max)) = (expected_end_min, expected_end_max) {
        if let Some(end) = metrics.end {
            assert!(
                end >= end_min && end <= end_max,
                "End time {} is not in range [{}, {}]",
                end,
                end_min,
                end_max
            );
        } else {
            panic!("Expected end time but metrics has none");
        }
    }
}

/// Assert that two events have the same span hierarchy
///
/// This checks that root_span_id matches between events
pub fn assert_same_trace(event1: &SpanEvent, event2: &SpanEvent) {
    assert_eq!(
        event1.root_span_id, event2.root_span_id,
        "Events are not part of the same trace"
    );
}

/// Helper to extract scores from an event and assert on them
pub fn assert_scores(event: &SpanEvent, expected_scores: HashMap<String, f64>) {
    let actual_scores = event.scores.as_ref()
        .expect("Event has no scores");

    for (key, expected_value) in expected_scores {
        let actual_value = actual_scores.get(&key)
            .unwrap_or_else(|| panic!("Score '{}' not found in event", key));

        assert_eq!(
            actual_value, &expected_value,
            "Score '{}' mismatch: expected {}, got {}",
            key, expected_value, actual_value
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_assert_json_matches_simple() {
        let actual = json!({"name": "test", "value": 42});
        let expected = json!({"name": "test"});

        assert_json_matches(&actual, &expected, "root");
    }

    #[test]
    fn test_assert_json_matches_nested() {
        let actual = json!({
            "outer": {
                "inner": {
                    "value": 123
                }
            }
        });
        let expected = json!({
            "outer": {
                "inner": {
                    "value": 123
                }
            }
        });

        assert_json_matches(&actual, &expected, "root");
    }

    #[test]
    #[should_panic(expected = "Expected key 'missing' not found")]
    fn test_assert_json_matches_missing_key() {
        let actual = json!({"name": "test"});
        let expected = json!({"missing": "key"});

        assert_json_matches(&actual, &expected, "root");
    }

    #[test]
    fn test_find_event_by_name() {
        let event1 = create_test_event("span1");
        let event2 = create_test_event("span2");

        let events = vec![event1, event2];

        let found = find_event_by_name(&events, "span2");
        assert!(found.is_some());
        assert_eq!(
            found.unwrap().span_attributes.as_ref().unwrap().name.as_ref().unwrap(),
            "span2"
        );
    }

    #[test]
    fn test_create_test_event() {
        let event = create_test_event("test-span");

        assert_eq!(
            event.span_attributes.as_ref().unwrap().name.as_ref().unwrap(),
            "test-span"
        );
        assert!(event.span_id.is_some());
        assert!(event.root_span_id.is_some());
        assert_eq!(event.project_id.as_ref().unwrap(), TEST_PROJECT_ID);
    }

    #[test]
    fn test_assert_parent_child_relationship() {
        let parent = create_test_event("parent");
        let parent_span_id = parent.span_id.clone().unwrap();

        let mut child = create_test_event("child");
        child.span_parents = Some(vec![parent_span_id.clone()]);

        assert_parent_child_relationship(&parent, &child);
    }

    #[test]
    fn test_assert_scores() {
        let mut event = create_test_event("test");
        let mut scores = HashMap::new();
        scores.insert("accuracy".to_string(), 0.95);
        scores.insert("latency".to_string(), 0.12);
        event.scores = Some(scores.clone());

        assert_scores(&event, scores);
    }
}
