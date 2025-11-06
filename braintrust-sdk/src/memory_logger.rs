//! In-memory logger for testing purposes
//!
//! This module provides a memory-based implementation of the event queue
//! that stores events in memory instead of sending them to the Braintrust API.
//! This is useful for testing and validating behavior without making actual API calls.
//!
//! # Example
//!
//! ```no_run
//! use braintrust_sdk::memory_logger::MemoryLogger;
//! use braintrust_sdk::{Logger, LoggerOptions};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create a memory logger
//!     let memory_logger = MemoryLogger::new();
//!
//!     // Use it with a Logger (implementation-specific setup)
//!     // ... logging operations ...
//!
//!     // Drain the events to inspect them
//!     let events = memory_logger.drain();
//!     println!("Captured {} events", events.len());
//!
//!     Ok(())
//! }
//! ```

use crate::types::SpanEvent;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// In-memory logger that stores events instead of sending them to the API
///
/// This is primarily used for testing. It captures all logged events in memory
/// and provides methods to inspect and retrieve them.
#[derive(Clone)]
pub struct MemoryLogger {
    /// Internal storage for events
    events: Arc<Mutex<Vec<SpanEvent>>>,
}

impl MemoryLogger {
    /// Create a new memory logger
    pub fn new() -> Self {
        MemoryLogger {
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Log an event to memory
    pub fn log(&self, event: SpanEvent) {
        let mut events = self.events.lock().unwrap();
        events.push(event);
    }

    /// Get the current count of logged events
    pub fn len(&self) -> usize {
        let events = self.events.lock().unwrap();
        events.len()
    }

    /// Check if the memory logger is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drain all events from the memory logger
    ///
    /// This returns all events and clears the internal storage.
    /// Events are merged by ID, simulating server-side behavior.
    pub fn drain(&self) -> Vec<SpanEvent> {
        let mut events = self.events.lock().unwrap();
        let drained: Vec<SpanEvent> = events.drain(..).collect();
        drop(events); // Release lock

        // Merge events by ID (simulates server behavior)
        merge_events_by_id(drained)
    }

    /// Get a copy of all events without draining
    ///
    /// This is useful for inspecting events without clearing them.
    /// Events are merged by ID, simulating server-side behavior.
    pub fn get_events(&self) -> Vec<SpanEvent> {
        let events = self.events.lock().unwrap();
        let cloned: Vec<SpanEvent> = events.clone();
        drop(events); // Release lock

        // Merge events by ID (simulates server behavior)
        merge_events_by_id(cloned)
    }

    /// Clear all stored events without returning them
    pub fn clear(&self) {
        let mut events = self.events.lock().unwrap();
        events.clear();
    }
}

impl Default for MemoryLogger {
    fn default() -> Self {
        Self::new()
    }
}

/// Merge events by their ID (row ID)
///
/// Events with the same ID are merged according to the is_merge flag.
/// - First event (is_merge=false) replaces all data
/// - Subsequent events (is_merge=true) merge field-by-field
///
/// This function simulates the server-side merging behavior.
fn merge_events_by_id(events: Vec<SpanEvent>) -> Vec<SpanEvent> {
    let mut event_map: HashMap<String, SpanEvent> = HashMap::new();

    for event in events {
        let id = event.id.clone();

        if let Some(existing) = event_map.get_mut(&id) {
            // Merge this event into the existing one
            existing.merge(&event);
        } else {
            // First event with this ID
            event_map.insert(id, event);
        }
    }

    // Return events as a Vec, maintaining insertion order would be ideal
    // but HashMap doesn't preserve order. For testing, order usually doesn't matter.
    event_map.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_memory_logger_creation() {
        let logger = MemoryLogger::new();
        assert_eq!(logger.len(), 0);
        assert!(logger.is_empty());
    }

    #[test]
    fn test_memory_logger_log() {
        let logger = MemoryLogger::new();
        let event = SpanEvent::new();
        logger.log(event);
        assert_eq!(logger.len(), 1);
        assert!(!logger.is_empty());
    }

    #[test]
    fn test_memory_logger_drain() {
        let logger = MemoryLogger::new();

        let mut event1 = SpanEvent::new();
        event1.input = Some(json!({"test": "data1"}));

        let mut event2 = SpanEvent::new();
        event2.input = Some(json!({"test": "data2"}));

        logger.log(event1);
        logger.log(event2);

        assert_eq!(logger.len(), 2);

        let drained = logger.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(logger.len(), 0);
        assert!(logger.is_empty());
    }

    #[test]
    fn test_memory_logger_get_events() {
        let logger = MemoryLogger::new();

        let event = SpanEvent::new();
        logger.log(event);

        // get_events shouldn't drain
        let events = logger.get_events();
        assert_eq!(events.len(), 1);
        assert_eq!(logger.len(), 1);

        // Should still be able to get them again
        let events2 = logger.get_events();
        assert_eq!(events2.len(), 1);
    }

    #[test]
    fn test_memory_logger_clear() {
        let logger = MemoryLogger::new();

        logger.log(SpanEvent::new());
        logger.log(SpanEvent::new());
        assert_eq!(logger.len(), 2);

        logger.clear();
        assert_eq!(logger.len(), 0);
        assert!(logger.is_empty());
    }

    #[test]
    fn test_merge_events_by_id() {
        let id = uuid::Uuid::new_v4().to_string();

        let mut event1 = SpanEvent::new();
        event1.id = id.clone();
        event1.input = Some(json!({"x": 1}));
        event1.is_merge = Some(false);

        let mut event2 = SpanEvent::new();
        event2.id = id.clone();
        event2.output = Some(json!({"y": 2}));
        event2.is_merge = Some(true);

        let merged = merge_events_by_id(vec![event1, event2]);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].input, Some(json!({"x": 1})));
        assert_eq!(merged[0].output, Some(json!({"y": 2})));
    }

    #[test]
    fn test_merge_multiple_different_ids() {
        let id1 = uuid::Uuid::new_v4().to_string();
        let id2 = uuid::Uuid::new_v4().to_string();

        let mut event1 = SpanEvent::new();
        event1.id = id1.clone();
        event1.input = Some(json!({"test": "event1"}));

        let mut event2 = SpanEvent::new();
        event2.id = id2.clone();
        event2.input = Some(json!({"test": "event2"}));

        let merged = merge_events_by_id(vec![event1, event2]);

        // Should have 2 separate events
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn test_merge_overwrites_when_not_is_merge() {
        let id = uuid::Uuid::new_v4().to_string();

        let mut event1 = SpanEvent::new();
        event1.id = id.clone();
        event1.input = Some(json!({"original": "data"}));
        event1.output = Some(json!({"original": "output"}));
        event1.is_merge = Some(false);

        let mut event2 = SpanEvent::new();
        event2.id = id.clone();
        event2.input = Some(json!({"new": "data"}));
        event2.is_merge = Some(false); // Not a merge, should replace

        let merged = merge_events_by_id(vec![event1, event2]);

        assert_eq!(merged.len(), 1);
        // event2 should completely replace event1
        assert_eq!(merged[0].input, Some(json!({"new": "data"})));
        assert_eq!(merged[0].output, None); // output was in event1, not event2
    }
}
