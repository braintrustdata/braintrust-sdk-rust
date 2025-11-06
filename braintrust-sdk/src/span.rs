//! Span implementation for hierarchical tracing

use crate::error::Result;
use crate::queue::EventQueue;
use crate::types::{SpanAttributes, SpanEvent, SpanMetrics, StartSpanOptions};
use chrono::Utc;
use serde_json::Value;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Trait representing a span in the tracing hierarchy
pub trait Span: Send + Sync {
    /// Log data to this span (merges with existing data)
    fn log(&self, data: Value) -> Result<()>;

    /// End the span and record the end time
    fn end(&self) -> Result<()>;

    /// Get the span ID
    fn id(&self) -> String;

    /// Get the span ID (for parent relationships)
    fn span_id(&self) -> String;

    /// Get the root span ID
    fn root_span_id(&self) -> String;

    /// Export the current span data
    fn export(&self) -> SpanEvent;
}

/// Internal span state
struct SpanState {
    event: SpanEvent,
    is_merge: bool,
    ended: bool,
}

/// Implementation of a span
pub struct SpanImpl {
    state: Arc<RwLock<SpanState>>,
    queue: Arc<EventQueue>,
}

impl SpanImpl {
    /// Create a new span
    pub(crate) fn new(
        queue: Arc<EventQueue>,
        options: StartSpanOptions,
        project_id: Option<String>,
    ) -> Arc<Self> {
        let span_id = Uuid::new_v4().to_string();
        let root_span_id = options
            .parent
            .as_ref()
            .map(|p| p.root_span_id.clone()) // Extract root_span_id from parent!
            .unwrap_or_else(|| span_id.clone());

        let mut event = SpanEvent::new();
        event.span_id = Some(span_id.clone());
        event.root_span_id = Some(root_span_id.clone());
        event.project_id = project_id;
        // Set log_id to "g" for project logs (required field)
        event.log_id = Some("g".to_string());

        // Set span attributes
        let mut span_attributes = SpanAttributes::default();
        if let Some(name) = options.name {
            span_attributes.name = Some(name);
        }
        if let Some(span_type) = options.span_type {
            span_attributes.span_type = Some(span_type);
        }
        event.span_attributes = Some(span_attributes);

        // Initialize metrics with start time
        let mut metrics = SpanMetrics::default();
        metrics.start = Some(Utc::now().timestamp_millis() as f64 / 1000.0);
        event.metrics = Some(metrics);

        // Set parent if provided
        if let Some(parent) = options.parent {
            event.span_parents = Some(vec![parent.span_id]);
        }

        // Merge initial event data if provided
        if let Some(initial_event) = options.event {
            event.merge(&initial_event);
        }

        let state = SpanState {
            event,
            is_merge: false,
            ended: false,
        };

        let span = Arc::new(SpanImpl {
            state: Arc::new(RwLock::new(state)),
            queue,
        });

        // Log the initial event (not a merge)
        span.flush_to_queue();

        // Switch to merge mode for subsequent logs
        {
            let mut state = span.state.write().unwrap();
            state.is_merge = true;
        }

        span
    }

    /// Flush the current span state to the queue
    fn flush_to_queue(&self) {
        let state = self.state.read().unwrap();
        let mut event = state.event.clone();
        event.is_merge = Some(state.is_merge);
        drop(state); // Release lock before enqueuing

        self.queue.enqueue(event);
    }

    /// Update the span's event data
    fn update_event<F>(&self, updater: F) -> Result<()>
    where
        F: FnOnce(&mut SpanEvent),
    {
        let mut state = self.state.write().unwrap();

        if state.ended {
            return Ok(()); // Silently ignore updates to ended spans
        }

        updater(&mut state.event);
        drop(state); // Release lock before flushing

        self.flush_to_queue();
        Ok(())
    }
}

impl Span for SpanImpl {
    fn log(&self, data: Value) -> Result<()> {
        self.update_event(|event| {
            // Parse the input data and merge it into the event
            if let Value::Object(map) = data {
                for (key, value) in map {
                    match key.as_str() {
                        "input" => event.input = Some(value),
                        "output" => event.output = Some(value),
                        "expected" => event.expected = Some(value),
                        "error" => {
                            event.error = value.as_str().map(|s| s.to_string());
                        }
                        "scores" => {
                            if let Value::Object(scores) = value {
                                let mut score_map = std::collections::HashMap::new();
                                for (k, v) in scores {
                                    if let Some(score) = v.as_f64() {
                                        score_map.insert(k, score);
                                    }
                                }
                                event.scores = Some(score_map);
                            }
                        }
                        "metadata" => event.metadata = Some(value),
                        "metrics" => {
                            if let Ok(metrics) = serde_json::from_value(value) {
                                event.metrics = Some(metrics);
                            }
                        }
                        _ => {
                            // Store unknown fields in metadata
                            if event.metadata.is_none() {
                                event.metadata = Some(Value::Object(serde_json::Map::new()));
                            }
                            if let Some(Value::Object(ref mut meta)) = event.metadata {
                                meta.insert(key, value);
                            }
                        }
                    }
                }
            }
        })
    }

    fn end(&self) -> Result<()> {
        self.update_event(|event| {
            let mut state = event.metrics.clone().unwrap_or_default();
            state.end = Some(Utc::now().timestamp_millis() as f64 / 1000.0);
            event.metrics = Some(state);
        })?;

        // Mark as ended
        let mut state = self.state.write().unwrap();
        state.ended = true;

        Ok(())
    }

    fn id(&self) -> String {
        let state = self.state.read().unwrap();
        state.event.id.clone()
    }

    fn span_id(&self) -> String {
        let state = self.state.read().unwrap();
        state
            .event
            .span_id
            .clone()
            .unwrap_or_else(|| state.event.id.clone())
    }

    fn root_span_id(&self) -> String {
        let state = self.state.read().unwrap();
        state
            .event
            .root_span_id
            .clone()
            .unwrap_or_else(|| state.event.span_id.clone().unwrap_or_else(|| state.event.id.clone()))
    }

    fn export(&self) -> SpanEvent {
        let state = self.state.read().unwrap();
        state.event.clone()
    }
}

/// Create a test span for testing purposes
///
/// This function is exposed publicly to allow tests to create spans
/// without going through the full Logger initialization.
pub fn create_test_span(
    queue: Arc<EventQueue>,
    options: StartSpanOptions,
    project_id: Option<String>,
) -> Arc<dyn Span> {
    SpanImpl::new(queue, options, project_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::EventQueue;

    #[tokio::test]
    async fn test_span_creation() {
        let queue = Arc::new(EventQueue::new(100));
        let options = StartSpanOptions::new().with_name("test-span");
        let span = SpanImpl::new(queue, options, Some("test-project".to_string()));

        assert!(!span.id().is_empty());
        assert!(!span.span_id().is_empty());
    }

    #[tokio::test]
    async fn test_span_log() {
        let queue = Arc::new(EventQueue::new(100));
        let options = StartSpanOptions::new().with_name("test-span");
        let span = SpanImpl::new(queue, options, Some("test-project".to_string()));

        let result = span.log(serde_json::json!({
            "input": "test input",
            "output": "test output"
        }));

        assert!(result.is_ok());

        let exported = span.export();
        assert_eq!(exported.input, Some(serde_json::json!("test input")));
        assert_eq!(exported.output, Some(serde_json::json!("test output")));
    }

    #[tokio::test]
    async fn test_span_end() {
        let queue = Arc::new(EventQueue::new(100));
        let options = StartSpanOptions::new().with_name("test-span");
        let span = SpanImpl::new(queue, options, Some("test-project".to_string()));

        let result = span.end();
        assert!(result.is_ok());

        let exported = span.export();
        assert!(exported.metrics.is_some());
        assert!(exported.metrics.unwrap().end.is_some());
    }
}
